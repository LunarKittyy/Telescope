# Telescope

Android app (`android/`, Kotlin) streams the phone camera to a Python/PyQt6 desktop app (`desktop/`) that feeds a virtual webcam. README.md is the user and protocol doc; `desktop/MODULES.md` describes each desktop module.

## Checks
- Desktop: `python -m pytest -q` and `python scripts/smoke_check.py` in `desktop/`
- Android: `./gradlew lintDebug testDebugUnitTest` in `android/`

## Commits and PRs
Luna reads every PR and commit. Write them to be skimmed, not to prove effort.

**Commit messages:** a subject line saying what changed, in plain words. Body only when the why isn't obvious from the diff, at most 3 short lines. No bullet list of every file touched.

**PR descriptions:** follow `.github/pull_request_template.md`.
- What changed and why in 2-5 bullets, one line each. Name user-visible behaviour first, internals only if a reviewer needs them.
- Checks: what you ran, one line each ("Desktop: 811 tests pass"). Don't list every new test.
- "Still needs a check on a real device": concrete things to try by hand, or "nothing".
- No section headers beyond the template's, no restating the diff, no background essay. If it runs past about 20 lines, cut it.

Plain, casual English. No em dashes.
