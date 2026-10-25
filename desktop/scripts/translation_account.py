"""Account erasure for the local workbench's explicitly configured cloud account."""

import base64
import json
import os
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


def anonymise(value, uid, username=None):
    identities = (uid, 'firebase:' + uid, 'github:' + username if username else None, username)
    if isinstance(value, dict):
        return {key: 'anonymous' if key in ('author', 'reviewer', 'authorUid', 'authorUsername', 'createdBy', 'grantedBy', 'updatedBy') and isinstance(item, str) and item in identities
                else anonymise(item, uid, username) for key, item in value.items()}
    if isinstance(value, list):
        return [anonymise(item, uid, username) for item in value]
    return value


def request(url, body=None, method=None, headers=None):
    call = urllib.request.Request(url, data=json.dumps(body).encode() if body is not None else None,
                                  method=method, headers={'Content-Type': 'application/json', **(headers or {})})
    with urllib.request.urlopen(call, timeout=30) as response:
        content = response.read()
        return json.loads(content) if content else {}


def verify_identity(id_token, api_key, recent=True, project='telescope-localization'):
    if not isinstance(id_token, str) or len(id_token) > 10000:
        raise ValueError('Sign in with GitHub again before deleting your account')
    try:
        data = request('https://identitytoolkit.googleapis.com/v1/accounts:lookup?key=' + api_key, {'idToken': id_token})
        user = data['users'][0]
        claims = json.loads(base64.urlsafe_b64decode(id_token.split('.')[1] + '==='))
    except (urllib.error.HTTPError, KeyError, IndexError, ValueError) as error:
        raise ValueError('Your sign-in could not be verified. Sign in with GitHub again.') from error
    if user.get('disabled') or not any(item.get('providerId') == 'github.com' for item in user.get('providerUserInfo', [])):
        raise ValueError('Use your GitHub account to request deletion')
    if claims.get('aud') != project or claims.get('iss') != 'https://securetoken.google.com/' + project:
        raise ValueError('Sign in to this translation project before managing your account')
    if claims.get('firebase', {}).get('sign_in_provider') != 'github.com' or claims.get('sub') != user['localId'] or (recent and not 0 <= time.time() - claims.get('auth_time', 0) <= 300):
        raise ValueError('Confirm your GitHub identity again before deleting your account')
    return user['localId']


class Store:
    def __init__(self, project, account=None):
        self.base = f'https://firestore.googleapis.com/v1/projects/{project}/databases/(default)/documents'
        self.name = f'projects/{project}/databases/(default)/documents'
        emulator = os.environ.get('FIRESTORE_EMULATOR_HOST')
        if emulator and project.startswith('demo-'):
            # The emulator accepts "owner" as an admin credential; demo- projects never reach Google
            self.base, self.headers = f'http://{emulator}/v1/{self.name}', {'Authorization': 'Bearer owner'}
            return
        if account is None:
            import google.auth
            from google.auth.transport.requests import Request
            credentials, _ = google.auth.default(scopes=['https://www.googleapis.com/auth/cloud-platform'])
            credentials.refresh(Request())
            self.headers = {'Authorization': 'Bearer ' + credentials.token}
            return
        try:
            token = subprocess.check_output(['gcloud', 'auth', 'print-access-token', '--project=' + project, '--account=' + account],
                                            text=True, stderr=subprocess.DEVNULL, timeout=30).strip()
        except (subprocess.SubprocessError, OSError) as error:
            raise ValueError('The account deletion service could not authenticate. Try again later.') from error
        self.headers = {'Authorization': 'Bearer ' + token}

    def call(self, path, body=None, method=None):
        return request(self.base + path, body, method, self.headers)

    def get(self, path):
        try:
            return self.call('/' + path)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            raise

    def put(self, path, fields):
        return self.call('/' + path, {'fields': fields}, 'PATCH')

    def delete(self, path):
        try:
            self.call('/' + path, method='DELETE')
        except urllib.error.HTTPError as error:
            if error.code != 404:
                raise

    def list(self, path, missing=False):
        result, page = [], ''
        while True:
            query = urllib.parse.urlencode({'pageSize': 1000, 'showMissing': str(missing).lower(), **({'pageToken': page} if page else {})})
            data = self.call('/' + path + '?' + query)
            result.extend(data.get('documents', []))
            page = data.get('nextPageToken')
            if not page:
                return result


def erase_data(store, uid):
    identity = store.get('identities/' + uid)
    username = identity['fields']['username']['stringValue'] if identity else None
    existing = store.get('maintenance/account-deletion')
    if existing and existing['fields']['uid']['stringValue'] != uid:
        raise ValueError('Another account deletion is in progress. Try again shortly.')
    if not existing:
        store.call(':commit', {'writes': [{'update': {'name': store.name + '/maintenance/account-deletion',
                   'fields': {'uid': {'stringValue': uid}}}, 'currentDocument': {'exists': False}}]})
    store.put('accountDeletions/' + uid, {'status': {'stringValue': 'pending'}})
    pointer = store.get('workspace/current')
    current_chunks, private_chunks, projection_chunks = [], [], []
    versions = store.list('versions', missing=True) + store.list('privateVersions', missing=True) + store.list('projections', missing=True)
    for version in versions:
        path = version['name'].split('/documents/')[1]
        chunks = sorted(store.list(path + '/chunks'), key=lambda item: int(item['name'].rsplit('/', 1)[1]))
        if not chunks:
            continue
        try:
            snapshot = json.loads(''.join(item['fields']['content']['stringValue'] for item in chunks))
        except json.JSONDecodeError:
            if pointer and path.rsplit('/', 1)[1] == pointer['fields']['version']['stringValue']:
                raise ValueError('The current project version is incomplete')
            for chunk in chunks:
                store.delete(chunk['name'].split('/documents/')[1])
            continue
        snapshot = anonymise(snapshot, uid, username)
        if path.startswith('projections/'):
            identities = (uid, 'firebase:' + uid, 'github:' + username if username else None, username)
            snapshot['approvals'] = {language: {entry: 'anonymous' if reviewer in identities else reviewer for entry, reviewer in reviewers.items()}
                                     for language, reviewers in snapshot.get('approvals', {}).items()}
        encoded = json.dumps(snapshot, ensure_ascii=False, separators=(',', ':'))
        size = max(1, (len(encoded) + len(chunks) - 1) // len(chunks))
        contents = [encoded[index * size:(index + 1) * size] for index in range(len(chunks))]
        for chunk, content in zip(chunks, contents):
            store.put(chunk['name'].split('/documents/')[1], {'content': {'stringValue': content}})
        if pointer and path.rsplit('/', 1)[1] == pointer['fields']['version']['stringValue']:
            if path.startswith('versions/'):
                current_chunks = contents
            elif path.startswith('projections/'):
                projection_chunks = contents
            else:
                private_chunks = contents
    github = 'github:' + username if username else uid
    for collection, field, value in [('submissions', 'authorUid', uid), ('submissions', 'reviewedBy', github), ('languages', 'createdBy', uid),
                                    ('languages', 'createdBy', github), ('roles', 'grantedBy', uid),
                                    ('approvals', 'author', github), ('approvals', 'reviewer', github),
                                    ('suggestions', 'author', github), ('suggestions', 'reviewedBy', github)]:
        while True:
            data = store.call(':runQuery', {'structuredQuery': {'from': [{'collectionId': collection}],
                              'where': {'fieldFilter': {'field': {'fieldPath': field}, 'op': 'EQUAL', 'value': {'stringValue': value}}}, 'limit': 100}})
            documents = [item['document'] for item in data if 'document' in item]
            if not documents:
                break
            fields = [field, 'authorUsername'] if field == 'authorUid' else [field]
            store.call(':commit', {'writes': [{'update': {'name': item['name'], 'fields': {name: {'stringValue': 'anonymous'} for name in fields}},
                                                'updateMask': {'fieldPaths': fields}} for item in documents]})
    if pointer and current_chunks:
        version = str(uuid.uuid4())
        for index, content in enumerate(current_chunks):
            store.put(f'versions/{version}/chunks/{index}', {'content': {'stringValue': content}})
        for index, content in enumerate(private_chunks):
            store.put(f'privateVersions/{version}/chunks/{index}', {'content': {'stringValue': content}})
        for index, content in enumerate(projection_chunks):
            store.put(f'projections/{version}/chunks/{index}', {'content': {'stringValue': content}})
        fields = pointer['fields']
        if projection_chunks:
            fields['projectionParts'] = {'integerValue': str(len(projection_chunks))}
        else:
            fields.pop('projectionParts', None)
        fields['version'] = {'stringValue': version}
        fields['updatedBy'] = {'stringValue': 'account-deletion'}
        store.put('workspace/current', fields)
    store.delete('profiles/' + uid)
    if username:
        store.delete('people/' + username)
    store.delete('roles/' + uid)
    store.delete('identities/' + uid)
    for kind in ('submissions', 'languages'):
        store.delete(f'limits/{uid}/actions/{kind}')


def delete_account(id_token, config, account):
    uid = verify_identity(id_token, config['apiKey'])
    store = Store(config['projectId'], account)
    try:
        erase_data(store, uid)
        request('https://identitytoolkit.googleapis.com/v1/accounts:delete?key=' + config['apiKey'], {'idToken': id_token})
        store.put('accountDeletions/' + uid, {'status': {'stringValue': 'complete'}})
    except (urllib.error.URLError, KeyError, ValueError) as error:
        raise ValueError('Account deletion did not finish. Retry to finish removing its data.') from error
    finally:
        lock = store.get('maintenance/account-deletion')
        if lock and lock['fields']['uid']['stringValue'] == uid:
            store.delete('maintenance/account-deletion')
