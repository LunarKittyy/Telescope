# Translation preview experiment

The [translation workbench](https://telescope-localization.web.app) tries translations in Telescope's own Qt widgets. About nine in ten desktop texts show on the screen where people read them, grouped into sections you pick from the language page:

- **Phone card**: cool and charging, warm, and stopped for heat.
- **Main window**: before any phone is paired, with a phone at three widths, streaming, starting, and with banners and footer lines.
- **Status lines and problems**: the Connection, Setup, Microphone, Updates, Stream output and other cards in the states that show their messages, one scene per state.
- **Settings**: one scene per page, plus the Plugins page with its questions and problems.
- **Dialogs and menus**: adding and listing phones, the shortcut editor, VA-API and adb setup, confirmation questions, and the tray and preset menus.
- **Notifications**: drawn as the desktop shows them, with a title and text.
- **Settings search**: the search box with one of a keyword's words typed in and the result it finds.
- **Other texts**: the few texts no screen shows yet, each with where the app uses them and sample values filled in. They show the wording and its length, not a screen.

No phone or virtual camera is needed. Android previews and plural editing aren't built yet, so the seven plural texts have no editor.

Use the [development environment](building.md#run-the-desktop-app-from-source), then run this in `desktop/`:

```bash
python scripts/translation_preview.py
```

Open the local address printed in the terminal. Choose a language from the overview and open a section. The list on the left has every text in that section, and picking one shows a screen with it, scrolled to where it sits at actual size (**Fit to screen** shows the whole screen). The arrows next to the screen list step through the other screens that show the same text. Select a text in the picture or list to edit it. A Qt process renders the scene after each edit, forked from one that has Python and Qt loaded so no state carries over between scenes, including custom-painted switches and readings. Highlights mark the containing widget rather than exact glyphs; selecting a tooltip highlights its owning control and opens its popup under it. Menu entries, tabs, list rows and dialog titles are highlighted by their own rectangle, and a text in a message box or input dialog marks the whole box. Missing or broken placeholders block the preview and export. A text that doesn't fit gets a red outline in the picture and a note below it. Suggesting or approving a wording also checks it on up to six other screens that show it, one per section and window width, narrowest first, and asks before going ahead when it doesn't fit somewhere. The wording that fills the box gets the same check, and a warning on its suggestion card.

The language bars show drafted texts in blue, approved texts in green and untranslated texts in grey. Only the owner and delegated admins approve translations. Edits are drafts as soon as you type. **Next** moves to the next text that isn't approved yet in the same section, then goes on to the next section.

Drafts save in browser storage. **Download .po** exports a copy with your edits. **Back up session** saves all browser drafts to a file, and **Restore a backup** loads them again in any browser. Removed source entries stay in that backup but are excluded from previews and .po exports.

## Shared project and accounts

Add `--shared` to enable GitHub sign-in and Firebase submissions. The manual editor loads no Firebase SDK until shared mode is chosen. GitHub is used for identity without requesting repository access. **Suggest** adds your wording to that text's suggestion list. Everyone signed in sees the list with each author's GitHub username and can start from a suggestion with **Use this**. Admins can suggest too, to ask the community first, and approve or dismiss suggestions from the same list or from Admin mode.

The machine translation of a text shows up in the list as **LLM Translate**, and everyone signed in can like or dislike any suggestion but their own. The translation box starts with the approved wording; until there is one, it starts with the suggestion with the best score (likes minus dislikes). A tie goes to the LLM Translate wording, or to a draft imported from a .po file where there's no machine wording, and then to the newest suggestion. Your own edits always win over both. Votes live in `votes/SUGGESTION_USERNAME`, one per person and suggestion, and the machine wordings are voted on as `llm-LANGUAGE-ENTRY`.

The account menu has a bio, language preferences, sign-out and an admin entry for authorised accounts. Profile language fields search by name or code and allow up to ten other languages. Only the owner can grant or revoke admins. Roles are enforced by Firestore rules, not by hiding buttons.

Contributors can add a language from the overview. The language list comes from Unicode CLDR and isn't limited to the catalogues already in Git. The workbench creates a blank catalogue with gettext plural rules where known. Unrecognised plural rules are marked for maintainer review. **Project settings** has a switch to pause contributor language creation. Ordinary accounts can add three languages per day and submit 100 suggestions per hour; admins can import larger changes.

GitHub usernames are the only contributor identities displayed or exported. Firebase account identifiers stay in private records used for permissions and pending suggestions. The platform doesn't copy email addresses or real names from GitHub into profiles or attribution. Bios and languages are public to signed-in translators through `people/USERNAME`, and clicking a name opens them. The private `profiles` record and `submissions` keep the Firebase account id for quotas and erasure; the public `suggestions` don't. Restored pending suggestions use separate admin-only storage.

Account erasure requires the local development server's `--cloud-account ACCOUNT` option and a fresh GitHub sign-in. The server verifies the Firebase token before using that explicitly selected gcloud account. It removes the profile, role and votes, anonymises submissions and identity references in saved project versions, then deletes the Firebase account. A private deletion marker blocks remaining old sign-in tokens. Failed cleanup can be retried. Already downloaded backups and Git history are outside the platform's control.

The hosted version uses a private Cloud Run worker and retryable Cloud Tasks jobs for erasure. It needs no maintainer login or downloaded service-account key. The public rendering service can enqueue verified account requests but has no Firestore data or Firebase Auth administration role. The private worker has those permissions and accepts authenticated service requests. Username verification uses the same queue and saves only the verified GitHub username.

## Updating texts and exporting approved languages

In a current checkout, create a repository snapshot:

```bash
python scripts/translation_preview.py --export-project /tmp/telescope-project.json
```

An admin can upload it under **Import, export & backups**, or use **Use this checkout** while running the workbench locally. **Preview import** shows the result before **Apply import** publishes it.

Imports compare the last imported Git snapshot, the platform's current version and the new snapshot. New strings are added. Removed strings are archived. Independent edits merge automatically; competing edits to the same field become conflicts. Source wording, context, plural or note changes invalidate approval. Moving source lines doesn't. Resolve a language's conflicts before exporting it.

**Export language** downloads a ZIP containing the full `.po` catalogue and its `translation-state/LANGUAGE.json` review record. Approved platform changes replace the imported Git wording; unapproved changes are excluded. Put the `.po` file in `desktop/telescope/locale/` and the review record in `desktop/translation-state/`, then review and commit both. The platform needs no GitHub write access.

**Back up whole project** includes the import base, working translations, pending suggestions, conflict records and archived strings. A backup can be restored into an empty project. Firebase saves stage immutable chunks before switching the current-version pointer. If another admin publishes first, the stale save fails and asks for a reload.

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

The browser contains the editor and an image with clickable regions. `scripts/translation_preview.py` runs the real window, Settings pages, dialogs and menus with sample state and uses Qt to capture the result. `scripts/translation_scenes.py` lists the sections and how scenes are built, and each area keeps its scenes in `translation_scenes_<area>.py`. A scene drives the real plugin through its own state, the event bus or `host.show_issue`, so a banner or status line is what the code builds. Nothing touches the network, the system or your own settings. Edits change an in-memory catalog before the widgets are constructed. A new render reads the current checkout; there are no stored screenshots or manually placed rectangles.

The worker temporarily instruments translation calls and the Qt setters for text, tooltips, titles, placeholders, combo box items, tabs, list rows, menu entries and message boxes to attach the translation key before Qt converts the text. Texts joined with `+` keep their keys; an f-string, `.format()` or a text passed into another `tr()` loses them, so a scene re-attaches those with a small helper in its own file. The same goes for custom-painted widgets: `StatusPill` has an adapter, and a scene marks other painted texts itself. It doesn't infer identity from translated wording, so two entries translated alike remain distinct. Modal dialogs are built without being run: the worker swaps `exec()` and the static message box helpers for ones that keep the dialog and return. Desktop notifications are captured and drawn as a bubble. These hooks exist only in the preview subprocess and don't change the installed app.

To add a screen, write a builder in the scenes file of its area that returns the shown widget and register it with `@scene` under a section. Import nothing from `telescope` at the top of that file: a module imported before the worker instruments `tr` keeps the plain one and its texts can't be mapped.

## Catching missing coverage

```bash
python scripts/translation_preview.py --check
python scripts/translation_preview.py --check-all
```

`--check` renders every scene and compares the texts each one shows with `scripts/translation_preview/coverage.json`. A new text, a text that stops showing in a scene, or a new gap fails the check. It renders about 650 scenes, which takes a few minutes of CPU, so `tests/test_translation_preview.py` samples ten scenes and checks the rest of the record for consistency. Set `TELESCOPE_FULL_PREVIEW=1` to make that test render every scene too. The container build runs the full `--check`. The local server renders everything once per checkout revision when it starts, so the first page load waits a moment.

`--check-all` also fails when a text has no scene at all. It passes today. The record lists the remaining gaps:

- **Plural texts** (`trn`) have no scene because the editor can't change plurals yet. There are seven.
- **Not on a real screen** lists texts that only appear on the Other texts pages, 125 of them. Some have no visible home (accessible names, replies printed to the terminal for `telescope --action`, log lines). The rest are states that were hard to reach and are the next candidates for real scenes.

A passing check means the declared coverage hasn't drifted, including those gaps. It doesn't mean every screen state, plural form or platform has been exercised. The previews render the host platform's Qt appearance, and the Windows-only code runs on Linux by patching the platform flags, so a Windows scene shows Windows wording in Linux fonts.

After inspecting affected scenes and deciding which remaining gaps are acceptable, run `python scripts/translation_preview.py --record-coverage` and review the changed record. Don't regenerate it automatically in CI. The renderer prevents an open editor from using a different app revision without a reload.

The development server listens only on `127.0.0.1` and accepts requests from that local origin. Each render has a timeout and a fresh worker. Hosted previews run the same Qt code inside a container with a restricted service identity. The container build renders the scenes and checks coverage before it can be deployed, and the hosted catalogue reads that recorded coverage instead of rendering every scene on a cold start.

## Updating the hosted site

Firebase Hosting serves the browser files and forwards `/api/` requests to Cloud Run. The existing deployment uses project `telescope-localization` and region `europe-west1`. Both services scale to zero; rendering is capped at two instances and account jobs at one. Billing is enabled for the project. Free allowances aren't a spending cap.

From `desktop/`, create a clean bundle in a new directory:

```bash
python -m scripts.translation_hosted --stage /tmp/telescope-deployment
```

Only runtime files are copied. The bundle excludes local settings, credentials, tests and private development notes. Build its container with `scripts/translation_preview/cloudbuild.yaml`, setting `_IMAGE` to a new tag in `europe-west1-docker.pkg.dev/telescope-localization/translations/workbench`. Select the intended Google account and project explicitly for every cloud command.

Update `translation-workbench` and `translation-erasure` to that image, then deploy Hosting and Firestore rules using the staged `firebase/firebase.json`. Hosting pins its API rewrite to the deployed preview revision. Keep the erasure service private and preserve its service identity and task audience configuration. Test GitHub sign-in, manual export and the real preview through the Hosting address after deployment.

Normal desktop tests cover the renderer, merge/export behaviour and hosted request checks. Firestore permission and erasure tests run against the local `demo-telescope` emulator with `FIRESTORE_EMULATOR_HOST=127.0.0.1:8085`. Never point those tests at the live project.

To try shared mode without real accounts, start both emulators and seed them with this checkout:

```bash
(cd scripts/translation_preview/firebase && firebase emulators:start --only firestore,auth --project demo-telescope)
FIRESTORE_EMULATOR_HOST=127.0.0.1:8085 python scripts/translation_sync.py --account emulator --project demo-telescope push --apply
FIRESTORE_EMULATOR_HOST=127.0.0.1:8085 python scripts/translation_preview.py --shared --emulators
```

Sign in through the emulator's form; the display name becomes the GitHub username. Give an account a role by writing `roles/UID` in the emulator UI. The push only reaches the emulator because `demo-` projects never go to Google. New languages also need `catalog/languageOptions` and a `languages/CODE` document, which the emulator UI can add.
