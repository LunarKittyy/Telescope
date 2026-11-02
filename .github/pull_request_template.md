<!-- What changed and why. If it fixes an issue, write "Closes #123". -->


## Checks
<!-- Tick what you ran, delete what doesn't apply to your change. -->
- [ ] Desktop: `cargo nextest run --workspace --locked` in `rust/`
- [ ] Android: `./gradlew lintDebug testDebugUnitTest` in `android/`
- [ ] README, `docs/` or `rust/ARCHITECTURE.md` updated, if something users see, the protocol or a crate's job changed

**Still needs a check on a real device:** <!-- what to try by hand before merging, or "nothing". If you did try it, say on what -->
