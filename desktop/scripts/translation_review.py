"""Repository-backed translation provenance and approvals, independent of the hosted service."""

import hashlib
import json
import os
import re
import string
import tempfile
from datetime import datetime, timezone
from pathlib import Path

from telescope import i18n

STATE_DIR = Path(__file__).resolve().parent.parent / 'translation-state'


def validate_text(source, text):
    def fields(value):
        result = []
        for _, name, spec, conversion in string.Formatter().parse(value):
            if name is None:
                continue
            if len(spec) > 64 or '{' in spec or '}' in spec or any(int(number) > 1000 for number in re.findall(r'\d+', spec)):
                raise ValueError('The translation has an unsupported placeholder format')
            result.append((name, spec, conversion))
        return sorted(result)
    if fields(source) != fields(text):
        raise ValueError('Keep every {placeholder} exactly as in the English')


def identity(entry):
    return hashlib.sha256(json.dumps([entry.ctx, entry.msgid], ensure_ascii=False).encode()).hexdigest()


def source_digest(entry):
    content = [entry.ctx, entry.msgid, entry.plural, entry.comments]
    return hashlib.sha256(json.dumps(content, ensure_ascii=False).encode()).hexdigest()


def digest(source, translated):
    content = [source.ctx, source.msgid, source.plural, source.comments, translated.msgstr]
    return hashlib.sha256(json.dumps(content, ensure_ascii=False).encode()).hexdigest()


def empty_state(language):
    return {'schema': 1, 'language': language, 'machine': {}, 'contributions': {}, 'approvals': {}}


def trusted_language(language, allowed):
    # Returns the code from the known list itself, so file names never come straight from a request
    match = next((code for code in allowed if code == language), None)
    if match is None:
        raise ValueError('Choose an available translation language')
    return match


def load(language):
    language = trusted_language(language, i18n.available())
    path = STATE_DIR / f'{language}.json'
    if not path.exists():
        return empty_state(language)
    state = json.loads(path.read_text(encoding='utf-8'))
    if state.get('schema') != 1 or state.get('language') != language:
        raise ValueError(f'Unsupported translation state in {path.name}')
    for section in ('machine', 'contributions', 'approvals'):
        if not isinstance(state.get(section), dict):
            raise ValueError(f'Invalid {section} in {path.name}')
    return state


def atomic_write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        stream.write(content)
        stream.flush()
        os.fsync(stream.fileno())
    try:
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def save(state):
    atomic_write(STATE_DIR / f"{trusted_language(state['language'], i18n.available())}.json", (json.dumps(state, indent=2, sort_keys=True) + '\n').encode())


def complete(source, translated):
    return bool(translated and not translated.fuzzy and translated.plural == source.plural
                and translated.msgstr and all(translated.msgstr))


def status(source, translated, state):
    if not complete(source, translated):
        return {'status': 'untranslated', 'origin': None}
    identifier, fingerprint = identity(source), digest(source, translated)
    approval = state['approvals'].get(identifier, {})
    contribution = state['contributions'].get(identifier, {})
    approved = approval.get('digest') == fingerprint and bool(approval.get('reviewer')) and bool(approval.get('approved_at'))
    if contribution.get('digest') == fingerprint:
        origin = contribution.get('origin', 'unknown')
    else:
        origin = 'machine' if state['machine'].get(identifier) == fingerprint else 'unknown'
    return {'status': 'approved' if approved else 'pending', 'origin': origin,
            'reviewer': approval.get('reviewer') if approved else None}


def approve(source, translated, state, reviewer):
    if not reviewer or not complete(source, translated):
        raise ValueError('A complete translation and a reviewer are required')
    for form in translated.msgstr:
        validate_text(source.msgid, form)
        if i18n.fields(form) != i18n.fields(source.msgid):
            raise ValueError('Every source placeholder must remain in an approved translation')
    state['approvals'][identity(source)] = {'digest': digest(source, translated), 'reviewer': reviewer,
                                           'approved_at': datetime.now(timezone.utc).isoformat()}


def import_po(language, content, sources, author):
    if not author or len(content) > 4_000_000:
        raise ValueError('An author and a .po file smaller than 4 MB are required')
    if any(int(match) > 5 for match in re.findall(r'(?m)^\s*(?:#~\s*)?msgstr\[(\d+)\]', content)):
        raise ValueError('The .po file has an invalid plural index')
    language = trusted_language(language, i18n.available())
    state = load(language)
    entries, header = i18n.read_po(i18n.LOCALE_DIR / f'{language}.po')
    source_by_key = {entry.key: entry for entry in sources.values()}
    with tempfile.TemporaryDirectory(prefix='telescope-po-import-') as folder:
        path = Path(folder) / 'incoming.po'
        path.write_text(content, encoding='utf-8')
        incoming, incoming_header = i18n.read_po(path)
    declared = i18n.header_field(incoming_header, 'Language')
    if declared and declared != language:
        raise ValueError('The .po language does not match the selected language')
    changes, seen = {}, set()
    count = i18n.plural_count(header)
    for entry in incoming:
        if entry.key in seen:
            raise ValueError('The .po file contains duplicate translation keys')
        seen.add(entry.key)
        source = source_by_key.get(entry.key)
        if source is None:
            raise ValueError('The .po file contains a text absent from the current source')
        if entry.plural != source.plural or len(entry.msgstr) != (count if source.plural else 1):
            raise ValueError('The .po file has incompatible plural forms')
        if entry.fuzzy or not all(entry.msgstr):
            continue
        for form in entry.msgstr:
            validate_text(source.msgid, form)
            if i18n.fields(form) != i18n.fields(source.msgid):
                raise ValueError(f'Keep every placeholder in {source.msgid!r}')
            try:
                form.format(**{field: 'example' for field in i18n.fields(source.msgid)})
            except (ValueError, KeyError, IndexError, AttributeError) as error:
                raise ValueError('The .po file contains invalid formatting') from error
        changes[entry.key] = entry
    changed = 0
    for entry in entries:
        incoming = changes.get(entry.key)
        if incoming is None or (entry.msgstr == incoming.msgstr and not entry.fuzzy):
            continue
        entry.msgstr = incoming.msgstr
        entry.flags = [flag for flag in entry.flags if flag != 'fuzzy']
        source = source_by_key[entry.key]
        identifier = identity(source)
        state['contributions'][identifier] = {'digest': digest(source, entry), 'author': author,
                                             'origin': 'human', 'submitted_at': datetime.now(timezone.utc).isoformat()}
        state['approvals'].pop(identifier, None)
        changed += 1
    with tempfile.TemporaryDirectory(prefix='telescope-po-import-') as folder:
        path = Path(folder) / 'merged.po'
        i18n.write_po(path, header, entries)
        content = path.read_bytes()
    save(state)
    atomic_write(i18n.LOCALE_DIR / f'{language}.po', content)
    return changed
