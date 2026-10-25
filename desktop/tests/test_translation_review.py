"""Repository approval records follow exact wording and reject contributor approval attempts."""

import copy
import json
from io import BytesIO
from types import SimpleNamespace

import pytest

from scripts import translation_review as review
from scripts import translation_preview as preview
from telescope import i18n


@pytest.fixture
def entries():
    source = i18n.Entry('Notify me')
    source.comments = ['A battery warning control']
    translated = copy.deepcopy(source)
    translated.msgstr = ['Notificarme']
    return source, translated


def test_machine_is_pending_until_admin_approval(entries):
    source, translated = entries
    state = review.empty_state('es')
    state['machine'][review.identity(source)] = review.digest(source, translated)
    assert review.status(source, translated, state)['origin'] == 'machine'
    assert review.status(source, translated, state)['status'] == 'pending'
    review.approve(source, translated, state, 'github:maintainer')
    assert review.status(source, translated, state)['status'] == 'approved'


@pytest.mark.parametrize('change', ['translation', 'source', 'context', 'plural', 'note', 'empty', 'fuzzy'])
def test_changes_cannot_keep_stale_approval(entries, change):
    source, translated = entries
    state = review.empty_state('es')
    review.approve(source, translated, state, 'github:maintainer')
    if change == 'translation':
        translated.msgstr = ['Different wording']
    elif change == 'source':
        source.msgid += '!'
    elif change == 'context':
        source.ctx = 'other'
    elif change == 'plural':
        source.plural = 'Notify them'
    elif change == 'note':
        source.comments.append('Only used for heat')
    elif change == 'empty':
        translated.msgstr = ['']
    else:
        translated.flags.append('fuzzy')
    assert review.status(source, translated, state)['status'] != 'approved'


def test_approval_survives_line_moves(entries):
    source, translated = entries
    state = review.empty_state('es')
    review.approve(source, translated, state, 'github:maintainer')
    source.locations = ['moved.py:999']
    assert review.status(source, translated, state)['status'] == 'approved'


def test_missing_state_never_implies_approval(entries, monkeypatch, tmp_path):
    monkeypatch.setattr(review, 'STATE_DIR', tmp_path)
    assert review.status(*entries, review.load('es'))['status'] == 'pending'


def test_empty_cannot_be_approved(entries):
    source, translated = entries
    translated.msgstr = ['']
    with pytest.raises(ValueError):
        review.approve(source, translated, review.empty_state('es'), 'github:maintainer')


@pytest.fixture
def local_catalog(tmp_path, monkeypatch, entries):
    source, translated = entries
    monkeypatch.setattr(i18n, 'LOCALE_DIR', tmp_path / 'locale')
    monkeypatch.setattr(review, 'STATE_DIR', tmp_path / 'state')
    i18n.LOCALE_DIR.mkdir()
    i18n.write_po(i18n.LOCALE_DIR / 'es.po', 'Language: es\nPlural-Forms: nplurals=2; plural=(n != 1);\n', [translated])
    return source, translated


def test_import_records_author_but_does_not_approve(local_catalog, tmp_path):
    source, translated = local_catalog
    state = review.empty_state('es')
    review.approve(source, translated, state, 'github:maintainer')
    review.save(state)
    translated.msgstr = ['Nueva traducción']
    imported = tmp_path / 'incoming.po'
    i18n.write_po(imported, 'Language: es\n', [translated])
    assert review.import_po('es', imported.read_text(encoding='utf-8'), {'text': source}, 'github:contributor') == 1
    state = review.load('es')
    assert review.status(source, translated, state)['status'] == 'pending'
    assert state['contributions'][review.identity(source)]['author'] == 'github:contributor'
    assert not state['approvals']


def test_malformed_import_changes_nothing(local_catalog, tmp_path):
    source, translated = local_catalog
    before = (i18n.LOCALE_DIR / 'es.po').read_bytes()
    translated.msgstr = ['{wrong}']
    imported = tmp_path / 'incoming.po'
    i18n.write_po(imported, 'Language: es\n', [translated])
    with pytest.raises(ValueError, match='placeholder'):
        review.import_po('es', imported.read_text(encoding='utf-8'), {'text': source}, 'github:contributor')
    assert (i18n.LOCALE_DIR / 'es.po').read_bytes() == before
    assert not (review.STATE_DIR / 'es.json').exists()


def test_repository_state_files_have_valid_fingerprints():
    for language in i18n.available():
        state = review.load(language)
        assert state['schema'] == 1
        assert all(len(key) == 64 and len(value) == 64 for key, value in state['machine'].items())


def test_manual_http_cannot_approve_or_import():
    for path in ('/api/approve', '/api/import-po'):
        handler = object.__new__(preview.Handler)
        handler.path = path
        payload = json.dumps({'lang': 'es', 'scene': 'cool', 'drafts': {}}).encode()
        handler.rfile = BytesIO(payload)
        handler.headers = {'Content-Type': 'application/json', 'Content-Length': str(len(payload))}
        handler.server = SimpleNamespace(reviewer=None)
        responses = []
        handler.allowed = lambda: True
        handler.reply = lambda status, body, mime: responses.append(status)
        handler.do_POST()
        assert responses == [403]


def test_admin_approval_writes_exact_wording_and_server_reviewer(local_catalog, monkeypatch):
    source, translated = local_catalog
    identity = preview.key(source.ctx, source.msgid)
    monkeypatch.setattr(preview, 'catalog', lambda: {identity: source})
    payload = json.dumps({'lang': 'es', 'scene': 'cool', 'drafts': {identity: 'Approved wording'},
                          'identity': identity, 'reviewer': 'forged-reviewer'}).encode()
    handler = object.__new__(preview.Handler)
    handler.path = '/api/approve'
    handler.rfile = BytesIO(payload)
    handler.headers = {'Content-Type': 'application/json', 'Content-Length': str(len(payload))}
    handler.server = SimpleNamespace(reviewer='github:maintainer')
    responses = []
    handler.allowed = lambda: True
    handler.reply = lambda status, body, mime: responses.append(status)
    handler.do_POST()
    assert responses == [200]
    translated = i18n.read_po(i18n.LOCALE_DIR / 'es.po')[0][0]
    assert translated.msgstr == ['Approved wording']
    result = review.status(source, translated, review.load('es'))
    assert result['status'] == 'approved'
    assert result['reviewer'] == 'github:maintainer'
