# Telescope

Android app (`android/`, Kotlin) streams the phone camera to a Rust/Slint desktop app (`rust/`) that feeds a virtual webcam. README.md is the landing page and Quick Start; everything else for users and contributors (features in `docs/features/`, troubleshooting, protocol, architecture, packaging and CI) is in `docs/`, indexed by `docs/README.md`.

Before changing how desktop plugins, sources or the window talk to each other, read `rust/ARCHITECTURE.md`. It explains why plugins don't depend on each other and why every plugin but Connection has to cope with being switched off. `rust/app/tests/architecture.rs` and `rust/app/tests/plugin_removal.rs` check both.

## Texts people see
Both apps are translatable. New UI text goes in `android/.../res/values/strings.xml` on the phone and through `tr()` (or `@tr()` in Slint) on the desktop, then `python tools/i18n.py extract` in `rust/`. `docs/development/translating.md` has the rules. Don't translate new texts, even though the `.po` and `values-*` entries look unfinished. They wait for the next translation pass (`automation/translation/`), which only runs when the owner asks for one. Old translations must stop showing the old meaning of an English text that changed. Delete that string from every `values-*/strings.xml` on the phone so it falls back to English (the desktop's `extract` already does this, since a changed text is a new entry and the old one remains as an obsolete entry).

## Trying a change on a real phone
`./gradlew installDev` in `android/` and `cargo run -p telescope-app -- dev --keep` in `rust/` give a dev app and dev desktop that pair with each other and leave the real installs alone (see `docs/development/building.md`).

## Checks
- Desktop: `cargo nextest run --workspace --locked` and `cargo clippy --workspace --locked --all-targets -- -D warnings` in `rust/`
- Android: `./gradlew lintDebug testDebugUnitTest` in `android/`

## Commits and PRs
Every PR and commit gets read by a person. Write them to be skimmed, not to prove effort.

Before anything goes out (a PR, commit messages, docs, release notes, UI text), go through your own writing checks and content rules in full, don't be lazy about it, even for a one-line change. Whatever text is visible to others is checked first.

Never write that an agent or a tool did something ("an agent reviewed this", "generated with..."). The work belongs to you and the person you work for and you're both responsible for it.

**Commit messages:** one short line in the imperative saying what the commit does ("Fix the preview freezing after a Wi-Fi drop"), under about 72 characters. No body, no Conventional Commits prefixes. If the why needs more than a line, it goes in the PR.

**PR descriptions:** follow `.github/pull_request_template.md` and the rules in `CONTRIBUTING.md`.
- Why: what someone using the app saw or was missing, then the cause in plain words, then why this fix. Never open with internals that need codebase knowledge.
- What changes for people using the app: up to about 4 one-line bullets, or "Nothing visible". Screenshots for UI changes, checked for QR codes, phone names, addresses and home paths.
- What could break and how to check it: the risky part and what to try on a real device, or "Nothing".
- If a line doesn't help someone understand or check the PR, delete it. No file lists, no test lists, no extra headings.

Plain, casual English. No em dashes. For PR descriptions, release notes, README and `docs/` edits and other prose people will read, use the `slopbeth` skill if it can be found locally.

## Watching PRs
React to what GitHub sends (CI results, review comments) but don't schedule recurring check-ins only to look at a PR. At most one check-in about 10 minutes after pushing, to see whether it's green and ready to merge.
