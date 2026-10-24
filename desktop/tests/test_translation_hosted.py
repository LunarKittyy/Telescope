"""The hosted transport keeps local privileges private and deletion jobs authenticated."""

import io
import json
import sys
from unittest.mock import Mock

import pytest

from scripts import translation_hosted as hosted


def call(path, payload=None, origin='https://telescope-localization.web.app', method=None):
    content = json.dumps(payload).encode() if payload is not None else b''
    environ = {'PATH_INFO': path, 'REQUEST_METHOD': method or ('POST' if payload is not None else 'GET'),
               'HTTP_ORIGIN': origin, 'CONTENT_TYPE': 'application/json', 'CONTENT_LENGTH': str(len(content)),
               'wsgi.input': io.BytesIO(content)}
    responses = []
    body = b''.join(hosted.application(environ, lambda status, headers: responses.append((status, dict(headers)))))
    return int(responses[0][0].split()[0]), body


def test_external_origin_is_rejected_before_work(monkeypatch):
    monkeypatch.setattr(hosted, 'catalogue', lambda: pytest.fail('Untrusted request ran the renderer'))
    assert call('/api/catalog', origin='https://other.example')[0] == 403


def test_hosted_catalogue_has_shared_mode_without_local_admin(monkeypatch):
    monkeypatch.setattr(hosted, 'catalogue', lambda: {'mode': 'manual', 'shared_available': True})
    status, body = call('/api/catalog')
    assert status == 200
    assert json.loads(body) == {'mode': 'manual', 'shared_available': True}


def test_local_approval_endpoint_cannot_write_in_hosted_mode():
    assert call('/api/approve', {'lang': 'es'})[0] == 403


def test_internal_task_endpoint_does_not_accept_public_requests(monkeypatch):
    monkeypatch.setattr(hosted, 'task_identity', lambda _: False)
    monkeypatch.setattr(hosted, 'complete_erasure', lambda _: pytest.fail('Unauthenticated deletion ran'))
    assert call('/internal/delete-account', {'uid': 'someone'})[0] == 403


def test_signed_task_runs_erasure_and_account_request_is_async(monkeypatch):
    deleted, queued = [], []
    monkeypatch.setattr(hosted, 'task_identity', lambda _: True)
    monkeypatch.setattr(hosted, 'complete_erasure', deleted.append)
    monkeypatch.setattr(hosted, 'queue_erasure', queued.append)
    assert call('/internal/delete-account', {'uid': 'test'})[0] == 200
    assert deleted == ['test']
    status, body = call('/api/delete-account', {'idToken': 'verified-elsewhere'})
    assert status == 202 and json.loads(body)['pending']
    assert queued == ['verified-elsewhere']


def test_failed_identity_never_queues_a_privileged_task(monkeypatch):
    monkeypatch.setattr(hosted.account, 'verify_identity', Mock(side_effect=ValueError('invalid')))
    monkeypatch.setattr(hosted.account, 'Store', lambda _: pytest.fail('Privileged credentials used'))
    assert call('/api/delete-account', {'idToken': 'invalid'})[0] == 400


def test_deployment_stage_contains_only_runtime_files(tmp_path):
    destination = tmp_path / 'stage'
    hosted.stage(destination)
    public = destination / 'firebase' / 'public'
    assert (public / 'index.html').is_file()
    assert (destination / 'desktop' / 'scripts' / 'translation_preview' / 'Dockerfile').is_file()
    assert not (public / 'firestore.rules').exists()
    assert not list(destination.rglob('CLAUDE.md'))
    assert not list(destination.rglob('*.pyc'))
    with pytest.raises(FileExistsError):
        hosted.stage(destination)


def test_identity_sync_only_persists_verified_github_username(monkeypatch):
    store = Mock()
    store.get.return_value = None
    monkeypatch.setattr(hosted.account,'Store',lambda _: store)
    replies = iter([{'users':[{'providerUserInfo':[{'providerId':'github.com','rawId':'123'}],
                               'email':'private@example.test','displayName':'Private Name'}]},
                    {'id':123,'login':'TestContributor','email':'private@example.test','name':'Private Name'}])
    monkeypatch.setattr(hosted.account,'request',lambda *args,**kwargs: next(replies))
    hosted.sync_identity('private-firebase-id')
    store.put.assert_called_once_with('identities/private-firebase-id',{'username':{'stringValue':'TestContributor'}})


def test_deleted_identity_cannot_be_recreated_by_an_old_job(monkeypatch):
    store = Mock()
    store.get.return_value = {'fields':{'status':{'stringValue':'complete'}}}
    monkeypatch.setattr(hosted.account,'Store',lambda _: store)
    monkeypatch.setattr(hosted.account,'request',lambda *args,**kwargs: pytest.fail('Deleted account was looked up'))
    hosted.sync_identity('deleted-user')
    store.put.assert_not_called()


def test_render_failure_does_not_expose_stack_traces(monkeypatch):
    monkeypatch.setattr(hosted.preview,'worker',Mock(side_effect=RuntimeError('Traceback (most recent call last): private server path')))
    status, body = call('/api/render',{'lang':'es'})
    assert status == 503
    assert b'Traceback' not in body and b'private server path' not in body


@pytest.mark.skipif(sys.platform == 'win32', reason='The hosted worker uses Linux resource limits')
def test_project_actions_run_in_a_bounded_worker():
    status, body = call('/api/project-action',{'action':'unsupported'})
    assert status == 400
    assert json.loads(body)['error'] == 'Choose a project action'


def test_busy_project_worker_is_rejected_without_queueing_more_work():
    assert hosted.project_slot.acquire(blocking=False)
    try:
        assert call('/api/project-action',{'action':'unsupported'})[0] == 429
    finally:
        hosted.project_slot.release()
