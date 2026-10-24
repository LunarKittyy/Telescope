# Translation preview experiment

The [translation workbench](https://telescope-localization.web.app) tries translations in Telescope's own Qt widgets. It currently has three scenes for the desktop's Phone card: cool and charging, warm, and stopped for heat. No phone or virtual camera is needed. Android previews and plural editing aren't built yet.

Use the [development environment](building.md#run-the-desktop-app-from-source), then run this in `desktop/`:

```bash
python scripts/translation_preview.py
```

Open the local address printed in the terminal. Choose a language from the overview, open the Phone editor and select a text in the picture or list to edit it. A fresh Qt process renders the scene after each edit, including custom-painted switches and readings. Highlights mark the containing widget rather than exact glyphs; selecting a tooltip highlights its owning control and shows its wording below the picture. Missing or broken placeholders block the preview and export. Text-fit findings appear below the picture.

The language bars show drafted texts in blue, approved texts in green and untranslated texts in grey. Only the owner and delegated admins approve translations. **Keep draft & next** prepares a local draft for export and never grants approval.

Drafts save in browser storage. **Download .po** exports a copy with your edits. **Back up session** saves all browser drafts (session import isn't built yet). Removed source entries stay in that backup but are excluded from previews and .po exports.

## Shared project and accounts

Add `--shared` to enable GitHub sign-in and Firebase submissions. The manual editor loads no Firebase SDK until shared mode is chosen. GitHub is used for identity without requesting repository access. Suggestions stay pending until an admin approves them.

The account menu has a bio, language preferences, sign-out and an admin entry for authorised accounts. Profile language fields search by name or code and allow up to ten other languages. Only the owner can grant or revoke admins. Roles are enforced by Firestore rules, not by hiding buttons.

Contributors can add a language from the overview. The language list comes from Unicode CLDR and isn't limited to the catalogues already in Git. The workbench creates a blank catalogue with gettext plural rules where known. Unrecognised plural rules are marked for maintainer review. **Project settings** has a switch to pause contributor language creation. Ordinary accounts can add three languages per day and submit 100 suggestions per hour; admins can import larger changes.

GitHub usernames are the only contributor identities displayed or exported. Firebase account identifiers stay in private records used for permissions and pending suggestions. The platform doesn't copy email addresses or real names from GitHub into profiles or attribution. Profiles are readable only by their owner and admins. Restored pending suggestions use separate admin-only storage.

Account erasure requires the local development server's `--cloud-account ACCOUNT` option and a fresh GitHub sign-in. The server verifies the Firebase token before using that explicitly selected gcloud account. It removes the profile and role, anonymises submissions and identity references in saved project versions, then deletes the Firebase account. A private deletion marker blocks remaining old sign-in tokens. Failed cleanup can be retried. Already downloaded backups and Git history are outside the platform's control.

The hosted version uses a private Cloud Run worker and retryable Cloud Tasks jobs for erasure. It needs no maintainer login or downloaded service-account key. The public rendering service can enqueue verified account requests but has no Firestore data or Firebase Auth administration role. The private worker has those permissions and accepts authenticated service requests. Username verification uses the same queue and saves only the verified GitHub username.

## Updating texts and exporting approved languages

In a current checkout, create a repository snapshot:

```bash
python scripts/translation_preview.py --export-project /tmp/telescope-project.json
```

An admin can upload it under **Import, export & backups**, or use **Use this checkout** while running the workbench locally. **Preview import** shows the result before **Apply import** publishes it.

Imports compare the last imported Git snapshot, the platform's current version and the new snapshot. New strings are added. Removed strings are archived. Independent edits merge automatically; competing edits to the same field become conflicts. Source wording, context, plural or note changes invalidate approval. Moving source lines doesn't. Resolve a language's conflicts before exporting it.

**Export language** downloads a ZIP containing the full `.po` catalogue and its `translation-state/LANGUAGE.json` review record. Approved platform changes replace the imported Git wording; unapproved changes are excluded. Put the `.po` file in `desktop/telescope/locale/` and the review record in `desktop/translation-state/`, then review and commit both. The platform needs no GitHub write access.

**Back up whole project** includes the import base, working translations, pending suggestions, conflict records and archived strings. A backup can be restored into an empty project. Imports, conflict fixes and `.po` imports save a new project version in chunks before switching the current-version pointer. If another admin publishes first, the stale save fails and asks for a reload.

Approving or dismissing a suggestion doesn't touch the project version. An approval is one small document in `approvals/LANGUAGE-ENTRY`, written in the same batch that marks the submission approved, so several admins can review at once. The editor reads a precomputed summary of the current version from `projections/` and lays newer approvals over it, so opening a language doesn't download the whole project. Exports and the next import fold those approvals into the project; an approval for a source text that changed since is skipped.

### Syncing from the command line

`scripts/translation_sync.py` does the same import and export without the admin page. It signs in through gcloud with the account you pass on each run, and doesn't save it anywhere.

```bash
python scripts/translation_sync.py --account ACCOUNT pull            # approved site wording into this checkout
python scripts/translation_sync.py --account ACCOUNT pull --lang pl  # one language, also one that's only on the site so far
python scripts/translation_sync.py --account ACCOUNT push            # show what importing this checkout would change
python scripts/translation_sync.py --account ACCOUNT push --apply    # publish that import
```

`pull` only changes translations and takes flags, comments and line numbers from the checkout. A language with no new approvals stays byte for byte the same. Languages that only exist on the site are skipped unless named with `--lang`. It also says how many suggestions are still waiting and whether the site's texts have fallen behind the checkout. `push` refuses to save if someone published in the meantime.

For local maintainer work without Firebase, `--admin PUBLIC_REVIEWER_NAME` enables approvals and `.po` import into this checkout. Imported wording remains pending until approved. This is a trusted local mode, not hosted authentication. See [the review-record format](../desktop/translation-state/README.md).

## How it follows the app

The browser contains the editor and an image with clickable regions. `scripts/translation_preview.py` builds the real `MonitoringPlugin` panel and its warning banner with sample state, then uses Qt to capture the result. Edits change an in-memory catalog before the widgets are constructed. A new render reads the current checkout; there are no stored screenshots or manually placed rectangles.

The worker temporarily instruments translation calls and widget setters to attach the translation key before Qt converts the text. It doesn't infer identity from translated wording. Two entries translated alike remain distinct. Custom-painted `StatusPill` readings have a small adapter. Other custom widgets will need adapters before they can claim coverage. These hooks exist only in the preview subprocess and don't change the installed app.

## Catching missing coverage

```bash
python scripts/translation_preview.py --check
python scripts/translation_preview.py --check-all
```

`--check` compares fresh scene mappings and the catalog against `scripts/translation_preview/coverage.json`. New Phone texts, removed mappings, per-scene mapping changes and changes to the texts outside this experiment fail the check. The same check runs in `tests/test_translation_preview.py` as part of the normal desktop suite. The browser computes fresh coverage too and lists the Phone texts still missing scenes.

`--check-all` fails until every catalog entry has a mapped preview. A normal `--check` passing means the declared coverage hasn't drifted, including its known gaps. It doesn't mean every screen, tooltip state, plural form or platform has been exercised. The prototype renders the host platform's Qt appearance at one width.

After inspecting affected scenes and deciding which remaining gaps are acceptable, run `python scripts/translation_preview.py --record-coverage` and review the changed record. Don't regenerate it automatically in CI. The renderer prevents an open editor from using a different app revision without a reload.

The development server listens only on `127.0.0.1` and accepts requests from that local origin. Each render has a timeout and a fresh worker. Hosted previews run the same Qt code inside a container with a restricted service identity. The container build renders the scenes and checks coverage before it can be deployed.

## Updating the hosted site

Firebase Hosting serves the browser files and forwards `/api/` requests to Cloud Run. The existing deployment uses project `telescope-localization` and region `europe-west1`. Both services scale to zero; rendering is capped at two instances and account jobs at one. Billing is enabled for the project. Free allowances aren't a spending cap.

From `desktop/`, create a clean bundle in a new directory:

```bash
python -m scripts.translation_hosted --stage /tmp/telescope-deployment
```

Only runtime files are copied. The bundle excludes local settings, credentials, tests and private development notes. Build its container with `scripts/translation_preview/cloudbuild.yaml`, setting `_IMAGE` to a new tag in `europe-west1-docker.pkg.dev/telescope-localization/translations/workbench`. Select the intended Google account and project explicitly for every cloud command.

Update `translation-workbench` and `translation-erasure` to that image, then deploy Hosting and Firestore rules using the staged `firebase/firebase.json`. Hosting pins its API rewrite to the deployed preview revision. Keep the erasure service private and preserve its service identity and task audience configuration. Test GitHub sign-in, manual export and the real preview through the Hosting address after deployment.

Normal desktop tests cover the renderer, merge/export behaviour and hosted request checks. Firestore permission and erasure tests run against the local `demo-telescope` emulator with `FIRESTORE_EMULATOR_HOST=127.0.0.1:8085`. Never point those tests at the live project.
