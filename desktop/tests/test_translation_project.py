"""Repository imports preserve platform work and expose divergent edits."""
import copy
import io
import json
import zipfile
import pytest
from scripts import translation_project as project
from scripts import translation_review as review
from telescope import i18n

@pytest.fixture
def repository():
    source = i18n.Entry('Notify me')
    identifier = review.identity(source)
    return {'schema': 1, 'kind': 'repository',
            'catalog': {identifier: {'english': source.msgid, 'context': '', 'plural': '', 'comments': [], 'locations': ['old.py:1']}},
            'languages': {'es': {'header': 'Language: es\nPlural-Forms: nplurals=2; plural=(n != 1);\n',
                                'entries': {identifier: {'forms': ['Notificarme'], 'fuzzy': False, 'machine': None,
                                                          'contribution': None, 'approval': None}}}}}

def first(snapshot):
    return next(iter(snapshot['languages']['es']['entries'].values()))

def test_git_addition_preserves_ahead_platform_approval(repository):
    identifier = next(iter(repository['catalog']))
    platform = project.approve(project.initialise(repository), 'es', identifier, 'Avisarme', 'owner', 'contributor')
    incoming = copy.deepcopy(repository)
    new = i18n.Entry('A new feature')
    new_id = review.identity(new)
    incoming['catalog'][new_id] = {'english': new.msgid, 'context': '', 'plural': '', 'comments': [], 'locations': []}
    incoming['languages']['es']['entries'][new_id] = {'forms': [''], 'fuzzy': False, 'machine': None, 'contribution': None, 'approval': None}
    merged = project.merge(platform, incoming)
    assert first(merged)['forms'] == ['Avisarme']
    assert first(merged)['approval'] == first(platform)['approval']
    assert new_id in merged['catalog']
    assert not merged['conflicts']
    assert first(repository)['forms'] == ['Notificarme']

def test_both_sides_editing_wording_requires_resolution(repository):
    platform = project.initialise(repository)
    first(platform)['forms'] = ['Platform wording']
    incoming = copy.deepcopy(repository)
    first(incoming)['forms'] = ['Git wording']
    merged = project.merge(platform, incoming)
    assert len(merged['conflicts']) == 1
    assert first(merged)['forms'] == ['Platform wording']
    with pytest.raises(ValueError, match='conflicts'):
        project.export_language(merged, 'es')
    resolved = project.resolve(merged, 0, 'repository')
    assert first(resolved)['forms'] == ['Git wording']
    assert not resolved['conflicts']

def test_source_note_change_invalidates_approval_without_losing_wording(repository):
    identifier = next(iter(repository['catalog']))
    platform = project.approve(project.initialise(repository), 'es', identifier, 'Avisarme', 'owner', 'contributor')
    incoming = copy.deepcopy(repository)
    incoming['catalog'][identifier]['comments'] = ['Now used for a different warning']
    merged = project.merge(platform, incoming)
    assert first(merged)['forms'] == ['Avisarme']
    assert first(merged)['approval'] is None
    assert not merged['conflicts']

def test_line_moves_keep_approval(repository):
    identifier = next(iter(repository['catalog']))
    platform = project.approve(project.initialise(repository), 'es', identifier, 'Avisarme', 'owner', 'contributor')
    incoming = copy.deepcopy(repository)
    incoming['catalog'][identifier]['locations'] = ['new.py:999']
    assert first(project.merge(platform, incoming))['approval'] is not None

def test_removed_strings_are_archived(repository):
    platform = project.initialise(repository)
    incoming = copy.deepcopy(repository)
    incoming['catalog'] = {}
    incoming['languages']['es']['entries'] = {}
    merged = project.merge(platform, incoming)
    assert not merged['catalog']
    assert merged['archive']['es']
    assert platform['catalog']

def test_export_only_approved_platform_edits_and_roundtrips_state(repository, tmp_path):
    identifier = next(iter(repository['catalog']))
    platform = project.initialise(repository)
    first(platform)['forms'] = ['Pending change']
    with zipfile.ZipFile(io.BytesIO(project.export_language(platform, 'es'))) as archive:
        assert 'Pending change' not in archive.read('locale/es.po').decode()
        assert 'Notificarme' in archive.read('locale/es.po').decode()
    approved = project.approve(platform, 'es', identifier, 'Avisarme', 'github:owner', 'github:contributor')
    with zipfile.ZipFile(io.BytesIO(project.export_language(approved, 'es'))) as archive:
        target = tmp_path / 'es.po'
        target.write_bytes(archive.read('locale/es.po'))
        state = json.loads(archive.read('translation-state/es.json'))
    source = project.source_entry(approved['catalog'][identifier])
    entry = i18n.read_po(target)[0][0]
    assert entry.msgstr == ['Avisarme']
    assert review.status(source, entry, state)['status'] == 'approved'

def test_repeated_repository_import_is_idempotent(repository):
    platform = project.initialise(repository)
    assert project.merge(platform, repository) == project.merge(project.merge(platform, repository), repository)

def test_unresolved_conflicts_block_next_import(repository):
    platform = project.initialise(repository)
    platform['conflicts'] = [{'language': 'es'}]
    with pytest.raises(ValueError, match='Resolve'):
        project.merge(platform, repository)


def test_new_platform_language_survives_git_update_and_exports_valid_po(repository, tmp_path):
    platform = project.add_language(project.initialise(repository), 'fr')
    identifier = next(iter(platform['catalog']))
    platform = project.approve(platform, 'fr', identifier, 'Me prévenir', 'owner', 'contributor')
    merged = project.merge(platform, repository)
    assert merged['languages']['fr']['entries'][identifier]['forms'] == ['Me prévenir']
    assert not merged['conflicts']
    with zipfile.ZipFile(io.BytesIO(project.export_language(merged, 'fr'))) as archive:
        path = tmp_path / 'fr.po'
        path.write_bytes(archive.read('locale/fr.po'))
        entries, header = i18n.read_po(path)
    assert entries[0].msgstr == ['Me prévenir']
    assert i18n.header_field(header, 'Language') == 'fr'


@pytest.mark.parametrize('language,count,n,expected', [('ja', 1, 200, 0), ('ar', 6, 0, 0),
    ('ar', 6, 2, 2), ('ru', 3, 21, 0), ('ru', 3, 22, 1), ('ru', 3, 25, 2), ('cy', 6, 6, 4)])
def test_new_language_plural_rules(language, count, n, expected):
    header = project.language_header(language)
    assert i18n.plural_count(header) == count
    assert i18n.plural_rule(header)(n) == expected


def test_import_rejects_extreme_plural_indices_before_parsing(repository):
    with pytest.raises(ValueError,match='plural index'):
        project.import_po(project.initialise(repository),'es','msgid "Notify me"\nmsgstr[1000000000] "test"\n','TestContributor')


def test_snapshot_rejects_extreme_plural_header(repository):
    repository['languages']['es']['header'] = 'Plural-Forms: nplurals=1000000000; plural=0;\n'
    with pytest.raises(ValueError,match='plural header'):
        project.validate(repository)


def test_git_adds_blank_catalogue_for_a_language_already_started_on_platform(repository):
    platform = project.add_language(project.initialise(repository),'fr')
    identifier = next(iter(platform['catalog']))
    platform = project.approve(platform,'fr',identifier,'Me prévenir','github:Owner','github:Contributor')
    incoming = project.add_language(repository,'fr')
    merged = project.merge(platform,incoming)
    assert not merged['conflicts']
    assert merged['languages']['fr']['entries'][identifier]['forms'] == ['Me prévenir']


def test_export_does_not_include_private_account_identifiers(repository):
    identifier = next(iter(repository['catalog']))
    platform = project.approve(project.initialise(repository),'es',identifier,'Avisarme','firebase:private-user','private@example.test')
    with zipfile.ZipFile(io.BytesIO(project.export_language(platform,'es'))) as archive:
        state = archive.read('translation-state/es.json').decode()
    assert 'private-user' not in state and 'private@example.test' not in state
    assert 'anonymous' in state


def test_firestore_approvals_fold_once_and_skip_changed_sources(repository):
    identifier = next(iter(repository['catalog']))
    platform = project.initialise(repository)
    digest = review.source_digest(project.source_entry(platform['catalog'][identifier]))
    approval = {'id': f'es-{identifier}', 'language': 'es', 'entry': identifier, 'sourceDigest': digest, 'text': 'Avisarme',
                'reviewer': 'github:Owner', 'author': 'github:Contributor', 'submission': '', 'approvedAt': '2026-10-25T10:00:00.000Z'}
    stale = {**approval, 'id': f'fr-{identifier}', 'language': 'fr', 'sourceDigest': '0' * 64}
    folded = project.apply_reviews(platform, [approval, stale], ['fr'])
    record = folded['languages']['es']['entries'][identifier]
    assert record['forms'] == ['Avisarme'] and record['approval']['approved_at'] == approval['approvedAt']
    assert folded['languages']['fr']['entries'][identifier]['forms'] == ['']
    assert project.projection(folded)['approvals']['es'] == {identifier: 'github:Owner'}
    record['forms'], record['approval'] = ['Notificarme'], None
    assert project.apply_reviews(folded, [approval])['languages']['es']['entries'][identifier]['forms'] == ['Notificarme']
    newer = project.apply_reviews(folded, [{**approval, 'approvedAt': '2026-10-26T10:00:00.000Z'}])
    assert newer['languages']['es']['entries'][identifier]['forms'] == ['Avisarme']
