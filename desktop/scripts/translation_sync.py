"""Sync the shared translation project with this checkout, using the same merge and export code as the admin page.

    python scripts/translation_sync.py --account ACCOUNT pull [--lang pl]
    python scripts/translation_sync.py --account ACCOUNT push [--apply]

`pull` writes the site's approved wording into `telescope/locale/` and `translation-state/`. `push` imports this
checkout's texts into the site and only saves with `--apply`. ACCOUNT is the gcloud account to authenticate with; it's
passed on each run and never stored.
"""

import argparse
import io
import json
import sys
import tempfile
import urllib.error
import uuid
import zipfile
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from scripts import translation_account as account  # noqa: E402
from scripts import translation_project as project  # noqa: E402
from scripts import translation_review as review  # noqa: E402
from telescope import i18n  # noqa: E402

PROJECT = 'telescope-localization'
CHUNK = 150000


def value(field):
    kind, content = next(iter(field.items()))
    if kind == 'integerValue':
        return int(content)
    if kind == 'timestampValue':
        return millis(content)
    if kind == 'nullValue':
        return None
    return content


def millis(stamp):
    # Firestore sends up to nanoseconds; the browser's Date keeps milliseconds, and folded approvals are compared as text
    whole, _, rest = stamp.rstrip('Z').partition('.')
    return f'{whole}.{(rest + "000")[:3]}Z'


def fields(document):
    return {name: value(field) for name, field in document.get('fields', {}).items()}


def split(text):
    pieces = [text[index:index + CHUNK] for index in range(0, len(text), CHUNK)]
    if len(pieces) > 200:
        raise ValueError('The project is too large to save')
    return pieces


def read_chunks(store, collection, version, parts):
    chunks = sorted(store.list(f'{collection}/{version}/chunks'), key=lambda item: int(item['name'].rsplit('/', 1)[1]))
    if len(chunks) != parts:
        raise ValueError(f'The saved {collection} for this version is incomplete')
    return ''.join(fields(chunk)['content'] for chunk in chunks)


def load(store):
    """The saved project with every approval and language created since folded in, plus what the save depends on."""
    pointer = store.get('workspace/current')
    approvals = [{**fields(item), 'id': item['name'].rsplit('/', 1)[1]} for item in store.list('approvals')]
    languages = [item['name'].rsplit('/', 1)[1] for item in store.list('languages')]
    if not pointer:
        return None, None, approvals, languages
    current = fields(pointer)
    platform = json.loads(read_chunks(store, 'versions', current['version'], current['parts']))
    return project.apply_reviews(platform, approvals, languages), pointer, approvals, languages


def merge_po(locale, language, exported):
    # Only the translations come from the site, so the checkout's flags, comments and line numbers stay as they are
    with tempfile.TemporaryDirectory(prefix='telescope-sync-') as folder:
        path = Path(folder) / 'site.po'
        path.write_bytes(exported)
        incoming, incoming_header = i18n.read_po(path)
    by_identity = {review.identity(entry): entry for entry in incoming}
    target = locale / f'{language}.po'
    entries, header = i18n.read_po(target if target.exists() else locale / 'telescope.pot')
    if not target.exists():
        header = incoming_header
    known = set()
    for entry in entries:
        identity = review.identity(entry)
        known.add(identity)
        found = by_identity.get(identity)
        if found is None:
            continue
        entry.msgstr = found.msgstr
        entry.flags = [flag for flag in entry.flags if flag != 'fuzzy'] + (['fuzzy'] if found.fuzzy else [])
    with tempfile.TemporaryDirectory(prefix='telescope-sync-') as folder:
        path = Path(folder) / f'{language}.po'
        i18n.write_po(path, header, entries)
        review.atomic_write(target, path.read_bytes())
    return len(known ^ set(by_identity))


def pull(store, languages=None, locale=None, state=None):
    locale, state = locale or i18n.LOCALE_DIR, state or review.STATE_DIR
    platform, _pointer, _approvals, _languages = load(store)
    if platform is None:
        raise ValueError('The shared project is empty. Push this checkout first.')
    pending = [fields(item) for item in store.list('submissions')]
    written = []
    for language in languages or sorted(platform['languages']):
        language = review.trusted_language(language, project.language_options())
        if language not in platform['languages']:
            print(f'{language}: not in the shared project')
            continue
        try:
            archive = zipfile.ZipFile(io.BytesIO(project.export_language(platform, language)))
        except ValueError as error:
            print(f'{language}: skipped. {error}')
            continue
        differences = merge_po(Path(locale), language, archive.read(f'locale/{language}.po'))
        review.atomic_write(Path(state) / f'{language}.json', archive.read(f'translation-state/{language}.json'))
        approved = len(json.loads(archive.read(f'translation-state/{language}.json'))['approvals'])
        waiting = sum(item.get('language') == language and item.get('status') == 'pending' for item in pending)
        print(f'{language}: {approved} approved' + (f', {waiting} suggestions still waiting on the site' if waiting else ''))
        if differences:
            print(f'{language}: {differences} texts differ between the site and this checkout. Run push to bring the site up to date.')
        written.append(language)
    return written


def push(store, snapshot, apply=False):
    platform, pointer, approvals, languages = load(store)
    merged = project.merge(platform, snapshot) if platform else project.initialise(snapshot)
    before, after = set(platform['catalog']) if platform else set(), set(merged['catalog'])
    conflicts = len(merged['conflicts']) - len(platform['conflicts'] if platform else [])
    print(f'{len(after - before)} new texts, {len(before - after)} removed texts archived, {conflicts} new conflicts to resolve on the site')
    if not apply:
        print('Nothing saved. Run again with --apply to publish this import.')
        return None
    version = str(uuid.uuid4())
    pieces = split(json.dumps(merged, ensure_ascii=False, separators=(',', ':')))
    projection = split(json.dumps(project.projection(merged), ensure_ascii=False, separators=(',', ':')))
    current = fields(pointer) if pointer else {}
    private = split(read_chunks(store, 'privateVersions', current['version'], current['privateParts'])) if current.get('privateParts') else []
    for collection, contents in (('versions', pieces), ('privateVersions', private), ('projections', projection)):
        for index, content in enumerate(contents):
            store.put(f'{collection}/{version}/chunks/{index}', {'content': {'stringValue': content}})
    now = datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.%fZ')
    write = {'update': {'name': f'{store.name}/workspace/current', 'fields': {
        'version': {'stringValue': version}, 'parts': {'integerValue': str(len(pieces))},
        'privateParts': {'integerValue': str(len(private))}, 'projectionParts': {'integerValue': str(len(projection))},
        'updatedBy': {'stringValue': 'repository-sync'}, 'updatedAt': {'timestampValue': now}}},
        'currentDocument': {'updateTime': pointer['updateTime']} if pointer else {'exists': False}}
    try:
        store.call(':commit', {'writes': [write]})
    except urllib.error.HTTPError as error:
        raise ValueError('Someone saved the project meanwhile. Nothing was published; run push again.') from error
    print(f'Published version {version}')
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--account', required=True, help='gcloud account to authenticate with (not stored)')
    parser.add_argument('--project', default=PROJECT, help='Firebase project id')
    commands = parser.add_subparsers(dest='command', required=True)
    pulling = commands.add_parser('pull', help="Write the site's approved wording into this checkout")
    pulling.add_argument('--lang', action='append', help='Only this language (repeatable)')
    pushing = commands.add_parser('push', help="Import this checkout's texts into the site")
    pushing.add_argument('--apply', action='store_true', help='Publish the import instead of only showing it')
    args = parser.parse_args()
    try:
        store = account.Store(args.project, args.account)
    except ValueError:
        print(f'Could not get a token for {args.project}. Check that `gcloud auth list` has that account.', file=sys.stderr)
        return 1
    try:
        if args.command == 'pull':
            pull(store, args.lang)
        else:
            from scripts.translation_preview import catalog
            push(store, project.repository_snapshot(catalog()), args.apply)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
