# Helping out with Telescope

Thanks for looking! You don't need to write code to help.

## Without code

- **Try it on your phone and let me know how it went.** Every phone behaves a bit differently and I only have a few to test on. Post in [Device compatibility](https://github.com/LunarKittyy/Telescope/discussions/new?category=device-compatibility), even if all you tried was one stream.
- **Report bugs** in an [issue](https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml). **Report a bug** under Updates & About in Settings (and in the phone app's About card) copies the diagnostics for you to paste in. Addresses, tokens and your home folder are stripped out of it.
- **Ideas and questions** go in [Discussions](https://github.com/LunarKittyy/Telescope/discussions).
- **Docs** that confused you are bugs too. A pull request that fixes a sentence is very welcome.

## With code

The Android app is in `android/` (Kotlin, Camera2) and the desktop app in `desktop/` (Python, PyQt6). [Architecture](docs/architecture.md) shows how they fit together and [desktop/MODULES.md](desktop/MODULES.md) goes through the desktop module by module.

To try a change on a real phone without touching your normal install:

```bash
cd android && ./gradlew installDev                 # a separate dev app on the phone
cd desktop && python scripts/dev_desktop.py --keep # a dev desktop that pairs with it
```

[Building and CI](docs/building.md) has the details.

Before opening a pull request, run the checks for what you changed:

- Desktop: `python -m pytest -q` and `python scripts/smoke_check.py` in `desktop/`
- Android: `./gradlew lintDebug testDebugUnitTest` in `android/`

Pull requests get read by a person, so keep them easy to skim. Commit subjects should say what changed in plain words. Fill in the [template](.github/pull_request_template.md) in a few lines, including what still needs trying on a real phone. If something users see changes, update `docs/` in the same pull request.

Telescope is [AGPL-3.0](LICENSE) and contributions are under the same license.
