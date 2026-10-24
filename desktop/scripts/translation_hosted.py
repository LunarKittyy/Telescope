"""WSGI transport for the hosted workbench and its retryable account-erasure job."""

import base64
import hashlib
import io
import json
import os
import subprocess
import sys
import threading
import urllib.error
from functools import lru_cache
from http import HTTPStatus
from types import SimpleNamespace

from scripts import translation_account as account
from scripts import translation_preview as preview

PROJECT = 'telescope-localization'
ORIGINS = {f'https://{PROJECT}.web.app', f'https://{PROJECT}.firebaseapp.com'}
catalogue_lock = threading.Lock()
project_slot = threading.BoundedSemaphore(1)


@lru_cache(maxsize=1)
def catalogue():
    return {**preview.metadata(), 'mode': 'manual', 'shared_available': True,
            'account_deletion_available': True}


def queue_account_job(id_token, action='delete-account'):
    config = json.loads((preview.ASSETS / 'firebase-config.json').read_text(encoding='utf-8'))
    uid = account.verify_identity(id_token, config['apiKey'], recent=action == 'delete-account')
    store = account.Store(PROJECT)
    region = os.environ.get('TASK_REGION', 'europe-west1')
    parent = f'projects/{PROJECT}/locations/{region}/queues/account-erasure'
    origin = os.environ['SERVICE_URL'].rstrip('/')
    identity = os.environ['TASK_SERVICE_ACCOUNT']
    task = {'name': parent + '/tasks/' + action + '-' + hashlib.sha256(uid.encode()).hexdigest(),
            'dispatchDeadline': '600s', 'httpRequest': {'httpMethod': 'POST',
            'url': origin + '/internal/' + action, 'headers': {'Content-Type': 'application/json'},
            'oidcToken': {'serviceAccountEmail': identity, 'audience': origin},
            'body': base64.b64encode(json.dumps({'uid': uid}).encode()).decode()}}
    try:
        account.request('https://cloudtasks.googleapis.com/v2/' + parent + '/tasks', {'task': task}, headers=store.headers)
    except urllib.error.HTTPError as error:
        if error.code != 409:
            raise ValueError('Account deletion could not be scheduled. Try again.') from error


def queue_erasure(id_token):
    queue_account_job(id_token)


def sync_identity(uid):
    import re
    store = account.Store(PROJECT)
    if store.get('accountDeletions/' + uid):
        return
    data = account.request(f'https://identitytoolkit.googleapis.com/v1/projects/{PROJECT}/accounts:lookup',
                           {'localId': [uid]}, headers=store.headers)
    users = data.get('users', [])
    if not users:
        return
    provider = next((item for item in users[0].get('providerUserInfo', []) if item['providerId'] == 'github.com'), None)
    if not provider or not str(provider.get('rawId', '')).isdigit():
        raise ValueError('A verified GitHub identity is required')
    raw_id = str(provider['rawId'])
    public = account.request('https://api.github.com/user/' + raw_id, headers={'Accept': 'application/vnd.github+json', 'User-Agent': 'Telescope-Translations'})
    username = public.get('login', '')
    if str(public.get('id')) != raw_id or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9-]{0,38}', username):
        raise ValueError('The GitHub username could not be verified')
    store.put('identities/' + uid, {'username': {'stringValue': username}})


def complete_erasure(uid):
    if not isinstance(uid, str) or not uid or len(uid) > 128 or '/' in uid:
        raise ValueError('Invalid account identifier')
    store = account.Store(PROJECT)
    marker = store.get('accountDeletions/' + uid)
    if marker and marker['fields']['status']['stringValue'] == 'complete':
        return
    try:
        account.erase_data(store, uid)
        try:
            account.request(f'https://identitytoolkit.googleapis.com/v1/projects/{PROJECT}/accounts:delete',
                            {'localId': uid}, headers=store.headers)
        except urllib.error.HTTPError as error:
            body = json.loads(error.read())
            if body.get('error', {}).get('message') != 'USER_NOT_FOUND':
                raise
        store.put('accountDeletions/' + uid, {'status': {'stringValue': 'complete'}})
    finally:
        lock = store.get('maintenance/account-deletion')
        if lock and lock['fields']['uid']['stringValue'] == uid:
            store.delete('maintenance/account-deletion')


def task_identity(environ):
    from google.auth.transport.requests import Request
    from google.oauth2.id_token import verify_oauth2_token
    token = environ.get('HTTP_AUTHORIZATION', '')
    if not token.startswith('Bearer '):
        return False
    try:
        claims = verify_oauth2_token(token[7:], Request(), audience=os.environ['SERVICE_URL'].rstrip('/'))
        return claims.get('email_verified') and claims.get('email') == os.environ['TASK_SERVICE_ACCOUNT']
    except (ValueError, KeyError):
        return False


class HostedRequest(preview.Handler):
    def __init__(self, environ):
        self.path = environ.get('PATH_INFO', '/')
        self.headers = {'Content-Type': environ.get('CONTENT_TYPE', ''),
                        'Content-Length': environ.get('CONTENT_LENGTH', '0'),
                        'Origin': environ.get('HTTP_ORIGIN', '')}
        self.rfile = environ['wsgi.input']
        self.server = SimpleNamespace(shared=True, reviewer=None, cloud_account=None)
        self.result = None

    def allowed(self):
        return self.headers['Origin'] in ORIGINS or not self.headers['Origin']

    def reply(self, status, data, content_type):
        if status == 400 and content_type == 'application/json' and b'Traceback (most recent call last)' in data:
            status, data = 503, b'{"error":"The preview is temporarily unavailable. Try again shortly."}'
        self.result = (status, data, content_type)


def application(environ, start_response):
    path, method = environ.get('PATH_INFO', '/'), environ['REQUEST_METHOD']
    status, body, mime = 404, b'Not found', 'text/plain'
    try:
        if path == '/health' and method == 'GET':
            status, body = 200, b'ok'
        elif path in ('/internal/delete-account', '/internal/identity') and method == 'POST':
            if not task_identity(environ):
                status, body = 403, b'Forbidden'
            else:
                length = int(environ.get('CONTENT_LENGTH') or '0')
                if not 0 < length <= 1000:
                    raise ValueError('Invalid task body')
                uid = json.loads(environ['wsgi.input'].read(length))['uid']
                if not isinstance(uid, str) or not uid or len(uid) > 128 or '/' in uid:
                    raise ValueError('Invalid account identifier')
                (complete_erasure if path.endswith('delete-account') else sync_identity)(uid)
                status, body = 200, b'complete'
        elif path.startswith('/api/'):
            request = HostedRequest(environ)
            if not request.allowed():
                status, body = 403, b'Forbidden origin'
            elif path == '/api/catalog' and method == 'GET':
                with catalogue_lock:
                    data = catalogue()
                status, body, mime = 200, json.dumps(data).encode(), 'application/json'
            elif path == '/api/project-action' and method == 'POST':
                length = int(request.headers['Content-Length'] or '0')
                if request.headers['Content-Type'] != 'application/json' or not 0 < length <= 32_000_000:
                    raise ValueError('Choose a JSON project request smaller than 32 MB')
                if not project_slot.acquire(timeout=20):
                    status, body, mime = 429, b'{"error":"The project service is busy. Try again shortly."}', 'application/json'
                else:
                    try:
                        result = subprocess.run([sys.executable,'-m','scripts.translation_hosted','--action'],
                                                input=request.rfile.read(length),capture_output=True,timeout=25)
                        if result.returncode:
                            raise ValueError('The project request exceeded its limits or could not be processed')
                        reply = json.loads(result.stdout)
                        status, body, mime = reply['status'], base64.b64decode(reply['body']), reply['mime']
                    finally:
                        project_slot.release()
            elif path in ('/api/delete-account', '/api/identity') and method == 'POST':
                if request.headers['Content-Type'] != 'application/json':
                    raise ValueError('Expected JSON')
                length = int(request.headers['Content-Length'] or '0')
                if not 0 < length <= 12000:
                    raise ValueError('Invalid request size')
                id_token = json.loads(request.rfile.read(length)).get('idToken')
                if path.endswith('delete-account'):
                    queue_erasure(id_token)
                else:
                    queue_account_job(id_token, 'identity')
                status, body, mime = 202, b'{"pending":true}', 'application/json'
            elif method in ('GET', 'POST'):
                (request.do_GET if method == 'GET' else request.do_POST)()
                status, body, mime = request.result
        elif method not in ('GET', 'POST'):
            status, body = 405, b'Method not allowed'
    except subprocess.TimeoutExpired:
        status, body, mime = 503, b'{"error":"The project request took too long. Try a smaller import."}', 'application/json'
    except (ValueError, KeyError, TypeError) as error:
        status, body, mime = 400, json.dumps({'error': str(error)}).encode(), 'application/json'
    except Exception:
        import logging
        logging.exception('Hosted translation request failed')
        status, body, mime = 503, b'{"error":"The service is temporarily unavailable. Try again shortly."}', 'application/json'
    start_response(f'{status} {HTTPStatus(status).phrase}', [('Content-Type', mime),
                   ('Content-Length', str(len(body))), ('Cache-Control', 'no-store'),
                   ('X-Content-Type-Options', 'nosniff')])
    return [body]


def stage(destination):
    import shutil
    destination.mkdir(parents=True, exist_ok=False)
    desktop = destination / 'desktop'
    desktop.mkdir()
    ignored = shutil.ignore_patterns('__pycache__', '*.pyc', '*.log', '_build.py')
    for folder in ('telescope', 'translation-state'):
        shutil.copytree(preview.ROOT / folder, desktop / folder, ignore=ignored)
    scripts = desktop / 'scripts'
    scripts.mkdir()
    for name in ('translation_preview', 'translation_project', 'translation_review', 'translation_account', 'translation_hosted', 'text_fit'):
        shutil.copy2(preview.ROOT / 'scripts' / (name + '.py'), scripts)
    shutil.copytree(preview.ASSETS, scripts / 'translation_preview', ignore=shutil.ignore_patterns('firebase', '__pycache__'))
    firebase = destination / 'firebase'
    firebase.mkdir()
    for name in ('firebase.json', 'firestore.rules', 'firestore.indexes.json', '.firebaserc'):
        shutil.copy2(preview.ASSETS / 'firebase' / name, firebase)
    public = firebase / 'public'
    public.mkdir()
    for name in ('index.html', 'app.js', 'shared.js', 'admin.js', 'language-picker.js', 'style.css', 'languages.json', 'languages.LICENSE.txt', 'firebase-config.json'):
        shutil.copy2(preview.ASSETS / name, public)


if __name__ == '__main__':
    import argparse
    from pathlib import Path
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_mutually_exclusive_group(required=True)
    commands.add_argument('--stage', type=Path, help='Create a clean deployment directory containing only runtime files')
    commands.add_argument('--action', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.stage:
        stage(args.stage)
    else:
        import resource
        resource.setrlimit(resource.RLIMIT_AS,(512 * 1024 * 1024,512 * 1024 * 1024))
        resource.setrlimit(resource.RLIMIT_CPU,(20,20))
        content = sys.stdin.buffer.read(32_000_001)
        if len(content) > 32_000_000:
            raise SystemExit(1)
        request = HostedRequest({'PATH_INFO':'/api/project-action','CONTENT_TYPE':'application/json',
                                 'CONTENT_LENGTH':str(len(content)),'wsgi.input':io.BytesIO(content)})
        request.do_POST()
        status, body, mime = request.result
        print(json.dumps({'status':status,'body':base64.b64encode(body).decode(),'mime':mime}))
