"""Portable project snapshots and three-way imports for the translation workbench."""

import copy
import io
import json
import re
import tempfile
import zipfile
from functools import lru_cache
from datetime import datetime, timezone
from pathlib import Path

from scripts import translation_review as review
from telescope import i18n


@lru_cache(maxsize=1)
def language_options():
    folder = Path(__file__).with_name('translation_preview')
    names = json.loads((folder / 'languages.json').read_text(encoding='utf-8'))
    return {code.replace('-', '_'): name for code, name in names.items() if code != 'en'}


def language_header(language):
    if language not in language_options():
        raise ValueError('Choose a supported language')
    rules = json.loads((Path(__file__).with_name('translation_preview') / 'plural-rules.json').read_text(encoding='utf-8'))['rules']
    rule = rules.get(language) or rules.get(language.split('_')[0])
    plural = f"nplurals={rule['forms']}; plural={rule['expression']};" if rule else 'nplurals=2; plural=(n != 1);'
    header = f'Project-Id-Version: Telescope\nLanguage: {language}\nMIME-Version: 1.0\nContent-Type: text/plain; charset=UTF-8\nContent-Transfer-Encoding: 8bit\nPlural-Forms: {plural}\n{ i18n.DRAFT_FIELD }: yes\n'
    if not rule:
        header += 'X-Telescope-Plural-Review: required\n'
    return header


def blank_record(source, header):
    return {'forms': [''] * (i18n.plural_count(header) if source.plural else 1), 'fuzzy': False,
            'machine': None, 'contribution': None, 'approval': None}


def add_language(platform, language):
    validate(platform)
    if language not in language_options():
        raise ValueError('Choose a supported language')
    result = copy.deepcopy(platform)
    if language not in result['languages']:
        header = language_header(language)
        result['languages'][language] = {'header': header, 'entries': {identifier: blank_record(source_entry(item), header) for identifier, item in result['catalog'].items()}}
    return result


def source_entry(item):
    entry = i18n.Entry(item['english'], ctx=item['context'], plural=item['plural'])
    entry.comments = item['comments']
    entry.locations = item.get('locations', [])
    return entry


def translated_entry(source, record):
    entry = copy.deepcopy(source)
    entry.msgstr = record['forms']
    entry.flags = ['fuzzy'] if record.get('fuzzy') else []
    return entry


def repository_snapshot(sources):
    catalog = {review.identity(entry): {'english': entry.msgid, 'context': entry.ctx, 'plural': entry.plural,
               'comments': entry.comments, 'locations': entry.locations} for entry in sources.values()}
    languages = {}
    for language in i18n.available():
        entries, header = i18n.read_po(i18n.LOCALE_DIR / f'{language}.po')
        found = {entry.key: entry for entry in entries}
        state = review.load(language)
        records = {}
        for identifier, item in catalog.items():
            source = source_entry(item)
            if any(not isinstance(value, str) or len(value) > 4000 for value in (source.msgid, source.ctx, source.plural)):
                raise ValueError('The project snapshot has an invalid source')
            target = found.get(source.key)
            records[identifier] = {'forms': target.msgstr if target else [''] * (i18n.plural_count(header) if source.plural else 1),
                                   'fuzzy': target.fuzzy if target else False,
                                   'machine': state['machine'].get(identifier),
                                   'contribution': state['contributions'].get(identifier),
                                   'approval': state['approvals'].get(identifier)}
        languages[language] = {'header': header, 'entries': records}
    return {'schema': 1, 'kind': 'repository', 'catalog': catalog, 'languages': languages}


def validate(snapshot):
    if not isinstance(snapshot, dict) or snapshot.get('schema') != 1:
        raise ValueError('Choose a version 1 Telescope project snapshot')
    if not isinstance(snapshot.get('catalog'), dict) or not isinstance(snapshot.get('languages'), dict):
        raise ValueError('The project snapshot has no catalogue or languages')
    if len(snapshot['catalog']) > 20000:
        raise ValueError('The project snapshot contains too many texts')
    for identifier, item in snapshot['catalog'].items():
        try:
            source = source_entry(item)
            if not all(isinstance(value, str) for value in (source.msgid, source.ctx, source.plural)) or review.identity(source) != identifier or not isinstance(source.comments, list) or not all(isinstance(note, str) for note in source.comments):
                raise ValueError('The project snapshot has an invalid source identity')
        except (KeyError, TypeError) as error:
            raise ValueError('The project snapshot has an invalid source') from error
    for language, data in snapshot['languages'].items():
        if language not in language_options() or not isinstance(data, dict) or not isinstance(data.get('entries'), dict):
            raise ValueError('The project snapshot contains an unsupported language')
        if not isinstance(data.get('header'), str):
            raise ValueError('The project snapshot has an invalid language header')
        count = i18n.plural_count(data['header'])
        if not 1 <= count <= 6 or len(data['header']) > 10000:
            raise ValueError('The project snapshot has an invalid plural header')
        for identifier, record in data['entries'].items():
            if identifier not in snapshot['catalog']:
                raise ValueError('A translation has no matching source')
            source = source_entry(snapshot['catalog'][identifier])
            if not isinstance(record, dict) or not isinstance(record.get('fuzzy'), bool):
                raise ValueError('The project snapshot has an invalid translation record')
            if any(record.get(field) is not None and not isinstance(record[field], dict) for field in ('approval', 'contribution')):
                raise ValueError('The project snapshot has an invalid review record')
            forms = record.get('forms')
            if not isinstance(forms, list) or len(forms) != (count if source.plural else 1):
                raise ValueError('The project snapshot has incompatible plural forms')
            for text in forms:
                if not isinstance(text, str) or len(text) > 4000:
                    raise ValueError('The project snapshot has an invalid translation')
                if text:
                    review.validate_text(source.msgid, text)
                    if i18n.fields(text) != i18n.fields(source.msgid):
                        raise ValueError('Keep every source placeholder in imported translations')
                    try:
                        text.format(**{field: 'example' for field in i18n.fields(source.msgid)})
                    except (ValueError, KeyError, IndexError, AttributeError) as error:
                        raise ValueError('An imported translation has invalid formatting') from error
    return snapshot


def initialise(repository):
    validate(repository)
    if repository.get('kind') != 'repository':
        raise ValueError('Initialise from a repository snapshot')
    return {**copy.deepcopy(repository), 'kind': 'platform', 'base': copy.deepcopy(repository),
            'archive': {}, 'conflicts': []}


def sanitise_approval(source, record):
    approval = record.get('approval') or {}
    target = translated_entry(source, record)
    if not review.complete(source, target) or approval.get('digest') != review.digest(source, target):
        record['approval'] = None


def merge(platform, incoming):
    validate(platform)
    validate(incoming)
    if incoming.get('kind') != 'repository' or platform.get('kind') != 'platform':
        raise ValueError('Import a repository snapshot into an existing platform project')
    if platform.get('conflicts'):
        raise ValueError('Resolve existing conflicts before importing another snapshot')
    base = validate(platform['base'])
    result = copy.deepcopy(platform)
    result['catalog'] = copy.deepcopy(incoming['catalog'])
    result['base'] = copy.deepcopy(incoming)
    incoming_languages = copy.deepcopy(incoming['languages'])
    for language, data in platform['languages'].items():
        if language not in incoming_languages:
            incoming_languages[language] = {'header': data['header'], 'entries': {identifier: copy.deepcopy(data['entries'].get(identifier)) or blank_record(source_entry(item), data['header']) for identifier, item in incoming['catalog'].items()}}
    for language, incoming_data in incoming_languages.items():
        current = platform['languages'].get(language, {'entries': {}})['entries']
        previous = base['languages'].get(language, {'entries': {}})['entries']
        merged = {}
        for identifier, remote in incoming_data['entries'].items():
            local, ancestor = current.get(identifier), previous.get(identifier)
            if local is None or local == ancestor:
                record = copy.deepcopy(remote)
            elif remote == ancestor or remote == local:
                record = copy.deepcopy(local)
            elif ancestor is None and not any(remote['forms']):
                record = copy.deepcopy(local)
            elif ancestor is None and not any(local['forms']):
                record = copy.deepcopy(remote)
            elif local['forms'] == remote['forms'] and local['fuzzy'] == remote['fuzzy']:
                record = copy.deepcopy(local)
            else:
                record = copy.deepcopy(local)
                result['conflicts'].append({'language': language, 'entry': identifier, 'field': 'translation',
                                            'base': ancestor, 'platform': local, 'repository': remote})
            sanitise_approval(source_entry(incoming['catalog'][identifier]), record)
            merged[identifier] = record
        removed = {identifier: {'source': platform['catalog'][identifier], 'translation': record}
                   for identifier, record in current.items() if identifier not in incoming['catalog']}
        result['archive'].setdefault(language, {}).update(copy.deepcopy(removed))
        result['languages'][language] = {'header': incoming_data['header'], 'entries': merged}
    return result


def resolve(platform, index, choice):
    if choice not in ('platform', 'repository'):
        raise ValueError('Choose the platform or repository version')
    result = copy.deepcopy(platform)
    try:
        conflict = result['conflicts'].pop(index)
    except (IndexError, TypeError) as error:
        raise ValueError('Choose an existing conflict') from error
    if conflict['field'] != 'translation':
        raise ValueError('Unsupported import conflict')
    record = conflict[choice]
    result['languages'][conflict['language']]['entries'][conflict['entry']] = record
    sanitise_approval(source_entry(result['catalog'][conflict['entry']]), record)
    return validate(result)


def export_language(platform, language):
    validate(platform)
    if any(item['language'] == language for item in platform.get('conflicts', [])):
        raise ValueError('Resolve this language’s conflicts before exporting')
    if language not in platform['languages']:
        raise ValueError('Choose an available language')
    language = review.trusted_language(language, language_options())
    data = platform['languages'][language]
    baseline = platform.get('base', platform)['languages'].get(language, {'entries': {}})['entries']
    state = review.empty_state(language)
    entries = []
    for identifier, item in platform['catalog'].items():
        source = source_entry(item)
        record = data['entries'][identifier]
        current = translated_entry(source, record)
        approval = record.get('approval') or {}
        approved = review.complete(source, current) and approval.get('digest') == review.digest(source, current)
        chosen = record if approved else baseline.get(identifier, blank_record(source, data['header']))
        target = translated_entry(source, chosen)
        entries.append(target)
        for section, field in [('machine', 'machine'), ('contributions', 'contribution'), ('approvals', 'approval')]:
            if chosen.get(field):
                state[section][identifier] = copy.deepcopy(chosen[field])
                if isinstance(state[section][identifier], dict):
                    for name in ('author','reviewer'):
                        if name in state[section][identifier] and not re.fullmatch(r'(?:github:)?[A-Za-z0-9][A-Za-z0-9-]{0,38}', state[section][identifier][name]):
                            state[section][identifier][name] = 'anonymous'
        if review.status(source, target, state)['status'] != 'approved':
            state['approvals'].pop(identifier, None)
    output = io.BytesIO()
    with tempfile.TemporaryDirectory(prefix='telescope-language-export-') as folder:
        path = Path(folder) / f'{language}.po'
        i18n.write_po(path, data['header'], entries)
        with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
            archive.writestr(f'locale/{language}.po', path.read_bytes())
            archive.writestr(f'translation-state/{language}.json', json.dumps(state, indent=2, sort_keys=True) + '\n')
    return output.getvalue()


def approve(platform, language, identifier, text, reviewer, author):
    validate(platform)
    result = copy.deepcopy(platform)
    if any(item['language'] == language and item['entry'] == identifier for item in result.get('conflicts', [])):
        raise ValueError('Resolve this text’s import conflict before approving')
    try:
        source = source_entry(result['catalog'][identifier])
        record = result['languages'][language]['entries'][identifier]
    except KeyError as error:
        raise ValueError('Choose a current source text and language') from error
    if source.plural:
        raise ValueError('Plural review is not available in this prototype')
    record['forms'], record['fuzzy'] = [text], False
    target = translated_entry(source, record)
    state = review.empty_state(language)
    review.approve(source, target, state, reviewer)
    record['approval'] = state['approvals'][identifier]
    record['contribution'] = {'digest': review.digest(source, target), 'author': author,
                              'origin': 'human', 'submitted_at': record['approval']['approved_at']}
    return validate(result)


def apply_reviews(platform, approvals=None, languages=None):
    # Folds approval documents and languages created in Firestore into a copy of the saved project
    validate(platform)
    approvals, languages = approvals or [], languages or []
    if not isinstance(approvals, list) or not isinstance(languages, list) or len(approvals) > 20000 or len(languages) > 500:
        raise ValueError('Choose valid approvals and languages')
    result = copy.deepcopy(platform)
    for language in languages:
        if isinstance(language, str) and language in language_options() and language not in result['languages']:
            header = language_header(language)
            result['languages'][language] = {'header': header, 'entries': {identifier: blank_record(source_entry(item), header) for identifier, item in result['catalog'].items()}}
    folded = result.setdefault('folded', {})
    conflicted = {(item['language'], item['entry']) for item in result.get('conflicts', [])}
    for item in approvals:
        if not isinstance(item, dict) or not all(isinstance(item.get(name), str) for name in ('id', 'language', 'entry', 'sourceDigest', 'text', 'reviewer', 'author', 'approvedAt')):
            continue
        if folded.get(item['id']) == item['approvedAt'] or (item['language'], item['entry']) in conflicted:
            continue
        try:
            source = source_entry(result['catalog'][item['entry']])
            record = result['languages'][item['language']]['entries'][item['entry']]
        except KeyError:
            continue
        if source.plural or review.source_digest(source) != item['sourceDigest']:
            continue
        target = copy.deepcopy(source)
        target.msgstr = [item['text']]
        state = review.empty_state(item['language'])
        try:
            review.approve(source, target, state, item['reviewer'])
        except ValueError:
            continue
        record['forms'], record['fuzzy'] = [item['text']], False
        record['approval'] = {**state['approvals'][item['entry']], 'approved_at': item['approvedAt']}
        record['contribution'] = {'digest': record['approval']['digest'], 'author': item['author'],
                                  'origin': 'machine' if item['author'] == 'machine' else 'human',
                                  'submitted_at': item['approvedAt']}
        folded[item['id']] = item['approvedAt']
    return validate(result)


def projection(platform):
    validate(platform)
    # contributed: unapproved wording a person wrote; the rest of the unapproved wording is the machine's
    result = {'translations': {}, 'approvals': {}, 'language_stats': {}, 'sources': {}, 'english': {}, 'contributed': {},
              'conflicts': copy.deepcopy(platform.get('conflicts', [])), 'folded': dict(platform.get('folded', {}))}
    for identifier, item in platform['catalog'].items():
        result['sources'][identifier] = review.source_digest(source_entry(item))
        result['english'][identifier] = item['english']
    for language, data in platform['languages'].items():
        translated, approvals, machine, contributed = {}, {}, 0, []
        for identifier, record in data['entries'].items():
            source = source_entry(platform['catalog'][identifier])
            target = translated_entry(source, record)
            state = review.empty_state(language)
            for section, field in [('machine', 'machine'), ('contributions', 'contribution'), ('approvals', 'approval')]:
                if record.get(field):
                    state[section][identifier] = record[field]
            status = review.status(source, target, state)
            if status['status'] != 'untranslated':
                translated[identifier] = target.msgstr[0]
            if status['status'] == 'approved':
                approvals[identifier] = status['reviewer']
            machine += status['origin'] == 'machine'
            if status['status'] == 'pending' and status['origin'] != 'machine':
                contributed.append(identifier)
        result['translations'][language], result['approvals'][language] = translated, approvals
        result['contributed'][language] = contributed
        result['language_stats'][language] = {'total': len(platform['catalog']), 'translated': len(translated), 'approved': len(approvals), 'machine': machine}
    return result


def import_po(platform, language, content, author):
    validate(platform)
    if language not in platform['languages'] or not isinstance(content, str) or len(content) > 4_000_000 or not author:
        raise ValueError('Choose a language, contributor name and .po file smaller than 4 MB')
    if not isinstance(author, str) or not re.fullmatch(r'(?:github:)?[A-Za-z0-9][A-Za-z0-9-]{0,38}', author):
        raise ValueError('Enter a GitHub username or Anonymous for attribution')
    result = copy.deepcopy(platform)
    if any(int(match) > 5 for match in re.findall(r'(?m)^\s*(?:#~\s*)?msgstr\[(\d+)\]', content)):
        raise ValueError('The .po file has an invalid plural index')
    with tempfile.TemporaryDirectory(prefix='telescope-project-po-') as folder:
        path = Path(folder) / 'incoming.po'
        path.write_text(content, encoding='utf-8')
        entries, header = i18n.read_po(path)
    if i18n.header_field(header, 'Language') not in ('', language):
        raise ValueError('The .po language does not match the selected language')
    seen = set()
    for incoming in entries:
        identifier = review.identity(incoming)
        if identifier not in result['catalog'] or identifier in seen:
            raise ValueError('The .po file contains duplicate or unknown source texts')
        seen.add(identifier)
        source = source_entry(result['catalog'][identifier])
        if incoming.plural != source.plural:
            raise ValueError('The .po file has incompatible plural forms')
        if not review.complete(source, incoming):
            continue
        record = result['languages'][language]['entries'][identifier]
        if record['forms'] == incoming.msgstr and not record['fuzzy']:
            continue
        record['forms'], record['fuzzy'], record['approval'] = incoming.msgstr, False, None
        record['contribution'] = {'author': author, 'origin': 'human', 'digest': review.digest(source, incoming),
                                  'submitted_at': datetime.now(timezone.utc).isoformat()}
    return validate(result)
