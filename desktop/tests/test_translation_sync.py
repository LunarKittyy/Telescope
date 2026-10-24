"""The maintainer sync reads and writes the shared project the way the admin page does."""
import copy
import io
import json
import urllib.error

import pytest
from scripts import translation_project as project
from scripts import translation_review as review
from scripts import translation_sync as sync
from telescope import i18n


class FakeStore:
    name = 'projects/demo/databases/(default)/documents'

    def __init__(self):
        self.documents, self.clock = {}, 0

    def _save(self, path, fields):
        self.clock += 1
        self.documents[path] = {'name': f'{self.name}/{path}', 'fields': fields, 'updateTime': f't{self.clock}'}

    def get(self, path):
        return copy.deepcopy(self.documents.get(path))

    def put(self, path, fields):
        self._save(path, fields)

    def list(self, path):
        depth = path.count('/') + 1
        return [copy.deepcopy(item) for key, item in sorted(self.documents.items()) if key.startswith(path + '/') and key.count('/') == depth]

    def call(self, path, body=None, method=None):
        assert path == ':commit'
        for write in body['writes']:
            target = write['update']['name'].removeprefix(self.name + '/')
            condition, existing = write.get('currentDocument', {}), self.documents.get(target)
            if ('updateTime' in condition and (not existing or existing['updateTime'] != condition['updateTime'])) or (condition.get('exists') is False and existing):
                raise urllib.error.HTTPError('', 400, 'FAILED_PRECONDITION', {}, io.BytesIO())
            self._save(target, write['update']['fields'])
        return {}


@pytest.fixture
def repository():
    source = i18n.Entry('Notify me')
    identifier = review.identity(source)
    return {'schema': 1, 'kind': 'repository',
            'catalog': {identifier: {'english': source.msgid, 'context': '', 'plural': '', 'comments': [], 'locations': []}},
            'languages': {'es': {'header': 'Language: es\nPlural-Forms: nplurals=2; plural=(n != 1);\n',
                                'entries': {identifier: {'forms': ['Notificarme'], 'fuzzy': False, 'machine': None,
                                                          'contribution': None, 'approval': None}}}}}


def approve(store, identifier, digest, text='Avisarme', language='es'):
    store.put(f'approvals/{language}-{identifier}', {name: {'stringValue': content} for name, content in
              {'language': language, 'entry': identifier, 'sourceDigest': digest, 'text': text,
               'reviewer': 'github:Owner', 'author': 'github:Contributor', 'submission': ''}.items()}
              | {'approvedAt': {'timestampValue': '2026-10-25T10:00:00.123456Z'}})
    store.put(f'languages/{language}', {'createdBy': {'stringValue': 'test'}})


def test_push_publishes_and_pull_writes_approved_wording(repository, tmp_path, capsys):
    store = FakeStore()
    assert sync.push(store, repository) is None
    assert store.get('workspace/current') is None
    version = sync.push(store, repository, apply=True)
    pointer = sync.fields(store.get('workspace/current'))
    assert pointer['version'] == version and pointer['projectionParts'] == 1 and pointer['updatedBy'] == 'repository-sync'
    identifier = next(iter(repository['catalog']))
    approve(store, identifier, review.source_digest(project.source_entry(repository['catalog'][identifier])))
    template = i18n.Entry('Notify me')
    template.flags, template.locations = ['python-brace-format'], ['new.py:9']
    i18n.write_po(tmp_path / 'telescope.pot', 'Language: es\nPlural-Forms: nplurals=2; plural=(n != 1);\n', [template])
    assert sync.pull(store, ['es'], tmp_path, tmp_path) == ['es']
    entries, _header = i18n.read_po(tmp_path / 'es.po')
    assert entries[0].msgstr == ['Avisarme']
    assert entries[0].flags == ['python-brace-format'] and entries[0].locations == ['new.py:9']
    state = json.loads((tmp_path / 'es.json').read_text(encoding='utf-8'))
    assert state['approvals'][identifier]['reviewer'] == 'github:Owner'
    assert 'es: 1 approved' in capsys.readouterr().out


def test_push_folds_site_approvals_and_keeps_private_backup(repository):
    store = FakeStore()
    sync.push(store, repository, apply=True)
    first = sync.fields(store.get('workspace/current'))
    store.put(f"privateVersions/{first['version']}/chunks/0", {'content': {'stringValue': '{"submissions":[]}'}})
    store.put('workspace/current', store.get('workspace/current')['fields'] | {'privateParts': {'integerValue': '1'}})
    identifier = next(iter(repository['catalog']))
    approve(store, identifier, review.source_digest(project.source_entry(repository['catalog'][identifier])))
    version = sync.push(store, repository, apply=True)
    saved = json.loads(sync.read_chunks(store, 'versions', version, 1))
    assert saved['languages']['es']['entries'][identifier]['forms'] == ['Avisarme']
    assert sync.read_chunks(store, 'privateVersions', version, 1) == '{"submissions":[]}'


def test_stale_push_publishes_nothing(repository, monkeypatch):
    store = FakeStore()
    sync.push(store, repository, apply=True)
    original = sync.load

    def raced(target):
        result = original(target)
        target.put('workspace/current', target.get('workspace/current')['fields'])
        return result
    monkeypatch.setattr(sync, 'load', raced)
    before = sync.fields(store.get('workspace/current'))['version']
    with pytest.raises(ValueError, match='saved the project meanwhile'):
        sync.push(store, repository, apply=True)
    assert sync.fields(store.get('workspace/current'))['version'] == before


def test_firestore_timestamps_match_the_browser_format():
    assert sync.millis('2026-10-25T10:00:00.123456789Z') == '2026-10-25T10:00:00.123Z'
    assert sync.millis('2026-10-25T10:00:00Z') == '2026-10-25T10:00:00.000Z'
