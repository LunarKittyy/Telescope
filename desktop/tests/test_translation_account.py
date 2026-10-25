"""Account deletion verifies a fresh identity before any privileged operation."""
import base64
import json
import time
import pytest
from scripts import translation_account as account

def encoded(claims):
    claims = {'aud':'telescope-localization','iss':'https://securetoken.google.com/telescope-localization',**claims}
    return 'header.' + base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip('=') + '.signature'

@pytest.mark.parametrize('uid,age,provider', [('someone-else', 0, 'github.com'), ('user', 301, 'github.com'), ('user', 0, 'password')])
def test_wrong_identity_old_auth_and_other_provider_are_rejected(monkeypatch, uid, age, provider):
    monkeypatch.setattr(account, 'request', lambda *args: {'users': [{'localId': 'user', 'providerUserInfo': [{'providerId': 'github.com'}]}]})
    token = encoded({'sub': uid, 'auth_time': time.time() - age, 'firebase': {'sign_in_provider': provider}})
    with pytest.raises(ValueError): account.verify_identity(token, 'public-key')

def test_recent_verified_github_identity_is_accepted(monkeypatch):
    monkeypatch.setattr(account, 'request', lambda *args: {'users': [{'localId': 'user', 'providerUserInfo': [{'providerId': 'github.com'}]}]})
    token = encoded({'sub': 'user', 'auth_time': time.time(), 'firebase': {'sign_in_provider': 'github.com'}})
    assert account.verify_identity(token, 'public-key') == 'user'


def test_valid_identity_from_another_firebase_project_is_rejected(monkeypatch):
    monkeypatch.setattr(account,'request',lambda *args: {'users':[{'localId':'user','providerUserInfo':[{'providerId':'github.com'}]}]})
    token = encoded({'sub':'user','aud':'another-project','auth_time':time.time(),'firebase':{'sign_in_provider':'github.com'}})
    with pytest.raises(ValueError,match='this translation project'):
        account.verify_identity(token,'public-key')

def test_anonymisation_covers_current_base_archived_and_approval_authors():
    data = {'languages': [{'author': 'firebase:user', 'forms': ['Keep my text']}],
            'base': {'reviewer': 'firebase:user'}, 'archive': {'authorUid': 'user'},
            'unrelated': {'author': 'firebase:another'}}
    result = account.anonymise(data, 'user')
    assert result['languages'][0] == {'author': 'anonymous', 'forms': ['Keep my text']}
    assert result['base']['reviewer'] == 'anonymous'
    assert result['archive']['authorUid'] == 'anonymous'
    assert result['unrelated']['author'] == 'firebase:another'
    assert data['base']['reviewer'] == 'firebase:user'
    assert account.anonymise({'author':'github:Example','reviewer':'github:Other'},'user','Example') == {'author':'anonymous','reviewer':'github:Other'}

def test_unverified_identity_never_opens_privileged_store(monkeypatch):
    monkeypatch.setattr(account, 'verify_identity', lambda *args: (_ for _ in ()).throw(ValueError('invalid')))
    monkeypatch.setattr(account, 'Store', lambda *args: pytest.fail('Unverified caller accessed the cloud store'))
    with pytest.raises(ValueError): account.delete_account('invalid', {'apiKey': 'public'}, 'unused')

@pytest.mark.skipif(not __import__('os').environ.get('FIRESTORE_EMULATOR_HOST'), reason='Requires the local demo Firestore emulator')
def test_erasure_keeps_submission_but_removes_identity_profile_role_and_version_credits():
    import os
    assert os.environ['FIRESTORE_EMULATOR_HOST'] == '127.0.0.1:8085'
    store = object.__new__(account.Store)
    store.base = 'http://127.0.0.1:8085/v1/projects/demo-telescope/databases/(default)/documents'
    store.name = 'projects/demo-telescope/databases/(default)/documents'
    store.headers = {'Authorization': 'Bearer owner'}
    uid = 'erase-test-user'
    store.put('roles/' + uid, {'role': {'stringValue': 'admin'}})
    store.put('profiles/' + uid, {'bio': {'stringValue': 'A private test bio'}})
    store.put('identities/' + uid, {'username': {'stringValue':'DeletedContributor'}})
    store.put('submissions/erase-test', {'authorUid': {'stringValue': uid}, 'text': {'stringValue': 'Keep this translation'}})
    payload = {'base': {'author': 'firebase:' + uid}, 'reviewer': 'firebase:' + uid, 'forms': ['Text']}
    store.put('versions/erase-test/chunks/0', {'content': {'stringValue': json.dumps(payload)}})
    store.put('privateVersions/erase-test/chunks/0', {'content': {'stringValue': json.dumps({'submissions':[{'authorUsername':'DeletedContributor','authorUid':uid}]})}})
    store.put('workspace/current', {'version': {'stringValue': 'erase-test'}, 'parts': {'integerValue': '1'}, 'privateParts':{'integerValue':'1'}})
    store.put('votes/llm-es-erase_DeletedContributor', {'voter': {'stringValue': 'github:DeletedContributor'}, 'value': {'integerValue': '1'}})
    try:
        account.erase_data(store, uid)
        assert store.get('profiles/' + uid) is None
        assert store.get('roles/' + uid) is None
        assert store.get('identities/' + uid) is None
        assert store.get('votes/llm-es-erase_DeletedContributor') is None
        submission = store.get('submissions/erase-test')['fields']
        assert submission['authorUid']['stringValue'] == 'anonymous'
        assert submission['text']['stringValue'] == 'Keep this translation'
        pointer = store.get('workspace/current')['fields']['version']['stringValue']
        assert pointer != 'erase-test'
        current = json.loads(store.get('versions/' + pointer + '/chunks/0')['fields']['content']['stringValue'])
        assert current['reviewer'] == 'anonymous'
        assert current['base']['author'] == 'anonymous'
        private = json.loads(store.get('privateVersions/' + pointer + '/chunks/0')['fields']['content']['stringValue'])
        assert private['submissions'][0] == {'authorUsername':'anonymous','authorUid':'anonymous'}
        account.erase_data(store, uid)
    finally:
        store.delete('maintenance/account-deletion')
