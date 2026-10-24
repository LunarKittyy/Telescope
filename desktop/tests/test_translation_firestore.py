"""Run against the demo Firestore emulator, never a live Firebase project."""

import base64
import json
import os
import time
import uuid
import urllib.error
import urllib.request

import pytest

HOST = os.environ.get('FIRESTORE_EMULATOR_HOST')
pytestmark = pytest.mark.skipif(not HOST, reason='Start the demo Firestore emulator to test deployed rules')
PROJECT = 'demo-telescope'
BASE = f'projects/{PROJECT}/databases/(default)/documents'


def token(uid='contributor', provider='github.com', admin=False):
    encode = lambda value: base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip('=')
    claims = {'aud': PROJECT, 'iss': f'https://securetoken.google.com/{PROJECT}', 'sub': uid,
              'iat': int(time.time()), 'exp': int(time.time()) + 3600,
              'firebase': {'sign_in_provider': provider}, 'admin': admin}
    return encode({'alg': 'none', 'typ': 'JWT'}) + '.' + encode(claims) + '.'


def request(path, body=None, credential=None, method=None):
    assert HOST in ('127.0.0.1:8085', 'localhost:8085'), 'Tests only permit the local demo emulator'
    headers = {'Content-Type': 'application/json'}
    if credential:
        headers['Authorization'] = 'Bearer ' + credential
    call = urllib.request.Request(f'http://{HOST}/v1/{BASE}{path}',
                                  data=json.dumps(body).encode() if body else None,
                                  headers=headers, method=method)
    try:
        with urllib.request.urlopen(call, timeout=10) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code


def submission(**changes):
    values = dict(language='es', entry='a' * 64, sourceDigest='b' * 64,
                  text='Una traducción', authorUid='contributor', authorUsername='TestContributor', status='pending')
    values.update(changes)
    return {'update': {'name': BASE + '/submissions/' + uuid.uuid4().hex,
                       'fields': {key: {'stringValue': value} for key, value in values.items()}},
            'updateTransforms': [{'fieldPath': 'createdAt', 'setToServerValue': 'REQUEST_TIME'}]}


def metered(write, kind='submissions', uid='contributor'):
    target = write['update']['name'].rsplit('/',1)[1]
    usage = {'update': {'name': BASE + f'/limits/{uid}/actions/{kind}',
                       'fields': {'count': {'integerValue':'1'}, 'lastTarget': {'stringValue':target}}},
             'updateTransforms':[{'fieldPath':'window','setToServerValue':'REQUEST_TIME'}]}
    return [usage,write]


@pytest.fixture(autouse=True)
def admin_roles():
    for uid, role in [('maintainer', 'admin'), ('project-owner', 'owner')]:
        fields = {'role': {'stringValue': role}, 'grantedBy': {'stringValue': 'test-bootstrap'}}
        assert request('/roles/' + uid, {'fields': fields}, 'owner', 'PATCH') == 200
    for uid, username in [('contributor','TestContributor'),('maintainer','TestMaintainer'),('project-owner','TestOwner')]:
        assert request('/identities/' + uid, {'fields': {'username': {'stringValue': username}}}, 'owner','PATCH') == 200
        for kind in ('submissions','languages'):
            request(f'/limits/{uid}/actions/{kind}',credential='owner',method='DELETE')
    from scripts import translation_project as project
    codes = {'arrayValue': {'values': [{'stringValue': code} for code in project.language_options()]}}
    assert request('/catalog/languageOptions', {'fields': {'codes': codes}}, 'owner', 'PATCH') == 200
    assert request('/languages/es', {'fields': {'createdBy': {'stringValue': 'test-bootstrap'}}}, 'owner', 'PATCH') == 200


def test_own_github_submission_waits_for_approval():
    write = submission()
    assert request(':commit', {'writes': metered(write)}, token()) == 200
    path = write['update']['name'].removeprefix(BASE)
    assert request(path, credential=token()) == 200
    assert request(path, credential=token('someone-else')) == 403
    assert request(path, credential=token('maintainer', admin=True)) == 200
    assert request(':commit', {'writes': metered(submission())}) == 403
    assert request(':commit', {'writes': metered(submission())}, token(provider='password')) == 403
    assert request(':commit', {'writes': [write]}, token()) == 403
    assert request(path, credential=token(), method='DELETE') == 403


@pytest.mark.parametrize('changes', [{'authorUid': 'someone-else'}, {'status': 'approved'},
                                    {'language': '../es'}, {'text': ''}, {'text': 'x' * 4001},
                                    {'entry': 'invalid'}, {'extra': 'unexpected'}, {'authorUsername':'Impersonated'}])
def test_forged_or_invalid_submissions_are_denied(changes):
    assert request(':commit', {'writes': metered(submission(**changes))}, token()) == 403


@pytest.mark.parametrize('collection', ['approvals', 'catalog'])
def test_clients_cannot_write_approvals_roles_or_catalogue(collection):
    write = {'update': {'name': BASE + f'/{collection}/test', 'fields': {'admin': {'booleanValue': True}}}}
    assert request(':commit', {'writes': [write]}, token('maintainer', admin=True)) == 403


def test_only_owner_can_grant_or_revoke_admin():
    fields = {'role': {'stringValue': 'admin'}, 'grantedBy': {'stringValue': 'project-owner'}}
    assert request('/roles/delegated', {'fields': fields}, token('project-owner'), 'PATCH') == 200
    assert request('/roles/another', {'fields': fields}, token('maintainer'), 'PATCH') == 403
    assert request('/roles/delegated', credential=token('maintainer'), method='DELETE') == 403
    assert request('/roles/delegated', credential=token('project-owner'), method='DELETE') == 200
    fields['role']['stringValue'] = 'owner'
    assert request('/roles/another', {'fields': fields}, token('project-owner'), 'PATCH') == 403


def test_only_admin_can_publish_platform_version():
    part = '/versions/' + uuid.uuid4().hex + '/chunks/0'
    content = {'fields': {'content': {'stringValue': '{}'}}}
    assert request(part, content, token(), 'PATCH') == 403
    assert request(part, content, token('maintainer'), 'PATCH') == 200
    assert request(part, content, token('maintainer'), 'PATCH') == 403
    assert request('/workspace/current', {'fields': {'version': {'stringValue': 'test'}}}, token(), 'PATCH') == 403


def test_missing_submission_can_be_checked_for_retry():
    assert request('/submissions/' + uuid.uuid4().hex, credential=token()) == 404


def test_profile_accepts_languages_outside_the_translation_catalogue():
    fields = {'bio': {'stringValue': 'A translator'}, 'nativeLanguage': {'stringValue': 'fr'},
              'languages': {'arrayValue': {'values': [{'stringValue': code} for code in ['de', 'ja', 'zh-Hant']]}}}
    assert request('/profiles/contributor', {'fields': fields}, token(), 'PATCH') == 200
    assert request('/profiles/someone-else', {'fields': fields}, token(), 'PATCH') == 403
    assert request('/profiles/contributor', credential=token('someone-else')) == 403
    fields['languages']['arrayValue']['values'] = [{'stringValue': 'en'}] * 11
    assert request('/profiles/contributor', {'fields': fields}, token(), 'PATCH') == 403
    fields['languages']['arrayValue']['values'] = [{'mapValue': {'fields': {}}}]
    assert request('/profiles/contributor', {'fields': fields}, token(), 'PATCH') == 403


def test_contributors_can_add_languages_until_admin_disables_creation():
    request('/languages/fr', credential='owner', method='DELETE')
    request('/languages/de', credential='owner', method='DELETE')
    request('/settings/project', credential='owner', method='DELETE')
    def create(code, uid='contributor'):
        username = 'TestMaintainer' if uid == 'maintainer' else 'TestContributor'
        write = {'update': {'name': BASE + '/languages/' + code, 'fields': {'createdBy': {'stringValue': 'github:' + username}}},
                 'updateTransforms': [{'fieldPath': 'createdAt', 'setToServerValue': 'REQUEST_TIME'}]}
        return request(':commit', {'writes': metered(write,'languages',uid)}, token(uid))
    assert create('fr') == 200
    assert create('xx') == 403
    disabled = {'fields': {'allowLanguageCreation': {'booleanValue': False}}}
    assert request('/settings/project', disabled, token(), 'PATCH') == 403
    assert request('/settings/project', disabled, token('maintainer'), 'PATCH') == 200
    assert create('de') == 403
    assert create('de', 'maintainer') == 200
    assert request(':commit', {'writes': metered(submission())}, token()) == 200
    request('/settings/project', credential='owner', method='DELETE')


def test_private_backup_and_identity_are_not_readable_by_other_contributors():
    path = '/privateVersions/' + uuid.uuid4().hex + '/chunks/0'
    assert request(path, {'fields': {'content': {'stringValue':'{"submissions":[]}'}}}, token('maintainer'),'PATCH') == 200
    assert request(path, credential=token()) == 403
    assert request(path, credential=token('maintainer')) == 200
    assert request('/identities/maintainer',credential=token()) == 403
    assert request('/identities/contributor',credential=token()) == 200
    assert request('/identities/contributor',{'fields':{'username':{'stringValue':'Forged'}}},token(),'PATCH') == 403


def test_submission_quota_cannot_be_skipped_reset_or_reused_for_a_batch():
    assert request(':commit',{'writes':[submission()]},token()) == 403
    write = submission()
    batch = metered(write) + [submission()]
    assert request(':commit',{'writes':batch},token()) == 403
    assert request(':commit',{'writes':metered(write)},token()) == 200
    assert request(':commit',{'writes':metered(submission())},token()) == 403
    limit = {'fields':{'count':{'integerValue':'100'},'lastTarget':{'stringValue':'already-used'},
                       'window':{'timestampValue': __import__('datetime').datetime.now(__import__('datetime').timezone.utc).isoformat()}}}
    assert request('/limits/contributor/actions/submissions',limit,'owner','PATCH') == 200
    writes = metered(submission())
    writes[0]['update']['fields']['count']['integerValue'] = '101'
    assert request(':commit',{'writes':writes},token()) == 403


def approval(**changes):
    values = dict(language='es', entry='a' * 64, sourceDigest='b' * 64, text='Avisarme',
                  reviewer='github:TestMaintainer', author='github:TestContributor', submission='')
    values.update(changes)
    return {'update': {'name': BASE + f"/approvals/{values['language']}-{values['entry']}",
                       'fields': {key: {'stringValue': value} for key, value in values.items()}},
            'updateTransforms': [{'fieldPath': 'approvedAt', 'setToServerValue': 'REQUEST_TIME'}]}


def reviewed(write, status='approved', reviewer='github:TestMaintainer'):
    return {'update': {'name': write['update']['name'], 'fields': {'status': {'stringValue': status}, 'reviewedBy': {'stringValue': reviewer}}},
            'updateMask': {'fieldPaths': ['status', 'reviewedBy']},
            'updateTransforms': [{'fieldPath': 'reviewedAt', 'setToServerValue': 'REQUEST_TIME'}]}


def test_admin_approval_is_one_batch_that_closes_the_submission():
    write = submission()
    assert request(':commit', {'writes': metered(write)}, token()) == 200
    identifier = write['update']['name'].rsplit('/', 1)[1]
    batch = [approval(submission=identifier), reviewed(write)]
    assert request(':commit', {'writes': batch}, token()) == 403
    assert request(':commit', {'writes': batch}, token('maintainer')) == 200
    assert request(':commit', {'writes': [reviewed(write, 'dismissed')]}, token('maintainer')) == 403
    assert request('/approvals/es-' + 'a' * 64, credential=token()) == 200
    assert request('/approvals/es-' + 'a' * 64, credential=token('maintainer'), method='DELETE') == 403


def test_admin_can_dismiss_but_not_rewrite_a_submission():
    write = submission()
    assert request(':commit', {'writes': metered(write)}, token()) == 200
    forged = reviewed(write, 'dismissed')
    forged['update']['fields']['text'] = {'stringValue': 'Changed'}
    forged['updateMask']['fieldPaths'].append('text')
    assert request(':commit', {'writes': [forged]}, token('maintainer')) == 403
    assert request(':commit', {'writes': [reviewed(write, 'dismissed', 'github:Someone')]}, token('maintainer')) == 403
    assert request(':commit', {'writes': [reviewed(write, 'pending')]}, token('maintainer')) == 403
    assert request(':commit', {'writes': [reviewed(write, 'dismissed')]}, token('maintainer')) == 200


@pytest.mark.parametrize('changes', [{'reviewer': 'github:Someone'}, {'language': 'xx'}, {'text': ''},
                                    {'entry': 'invalid'}, {'extra': 'unexpected'}])
def test_forged_or_invalid_approvals_are_denied(changes):
    assert request(':commit', {'writes': [approval(**changes)]}, token('maintainer')) == 403


def test_approval_name_must_match_its_language_and_text():
    write = approval()
    write['update']['name'] = BASE + '/approvals/es-' + 'c' * 64
    assert request(':commit', {'writes': [write]}, token('maintainer')) == 403
