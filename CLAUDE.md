# Telescope

Android app (`android/`, Kotlin) streams the phone camera to a Python/PyQt6 desktop app (`desktop/`) that feeds a virtual webcam. README.md is the landing page and Quick Start; everything else for users and contributors (features, troubleshooting, protocol, architecture, CI) is in `docs/`, indexed by `docs/README.md`; `desktop/MODULES.md` describes each desktop module.

## Trying a change on a real phone
`./gradlew installDev` in `android/` and `python scripts/dev_desktop.py --keep` in `desktop/` give a dev app and dev desktop that pair with each other and leave the real installs alone (see `docs/building.md`).

## Checks
- Desktop: `python -m pytest -q` and `python scripts/smoke_check.py` in `desktop/`
- Android: `./gradlew lintDebug testDebugUnitTest` in `android/`

## Commits and PRs
Every PR and commit gets read by a person. Write them to be skimmed, not to prove effort.

**Commit messages:** a subject line saying what changed, in plain words. Body only when the why isn't obvious from the diff, at most 3 short lines. No bullet list of every file touched.

**PR descriptions:** follow `.github/pull_request_template.md`.
- What changed and why in 2-5 bullets, one line each. Name user-visible behaviour first, internals only if a reviewer needs them.
- Checks: what you ran, one line each ("Desktop: 811 tests pass"). Don't list every new test.
- "Still needs a check on a real device": concrete things to try by hand, or "nothing".
- No section headers beyond the template's, no restating the diff, no background essay. If it runs past about 20 lines, cut it.

Plain, casual English. No em dashes. For PR descriptions, release notes, README and `docs/` edits and other prose people will read, use the `slopbeth` skill if it can be found locally.

## Watching PRs
React to what GitHub sends (CI results, review comments), but don't schedule recurring check-ins just to look at a PR. At most one check-in about 10 minutes after pushing, to see whether it's green and ready to merge.
