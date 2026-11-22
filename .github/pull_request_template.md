## Why
<!-- What someone using the app saw or was missing, then the cause in plain words, then why this fix.
Example: "The video turned into grey smears after a Wi-Fi drop. The decoder carried on without a keyframe. This requests a new one from the phone."
Technical detail can come after, in brief. If it fixes an issue, write "Closes #123". -->


## What changes for people using the app
<!-- Up to about 4 one-line bullets, or "Nothing visible". For a UI change, add before and after screenshots.
Check first that they show no pairing QR codes, phone names, IP addresses or home folder paths. -->


## What could break and how to check it
<!-- The risky part a reviewer should look at closely, and what to try on a real phone or computer before merging. "Nothing" if it's internal. If you did try it, say on what. -->


## Checks
<!-- Tick what you ran, delete what doesn't apply. -->
- [ ] Desktop: `cargo nextest run --workspace --locked` and `cargo clippy --workspace --locked --all-targets -- -D warnings` in `rust/`
- [ ] Android: `./gradlew lintDebug testDebugUnitTest` in `android/`
- [ ] README, `docs/` or `rust/ARCHITECTURE.md` updated, if something users see, the protocol or a crate's job changed

<!-- If a line doesn't help someone understand or check this pull request, delete it. No file lists, no test lists, no extra headings. -->
