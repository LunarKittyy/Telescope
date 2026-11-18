# Helping out with Telescope

Thanks for looking! You don't need to write code to help.

## Without code

- **Try it on your phone and let me know how it went.** Every phone behaves a bit differently and I only have a few to test on. Post in [Device compatibility](https://github.com/LunarKittyy/Telescope/discussions/new?category=device-compatibility), even if all you tried was one stream.
- **Report bugs** in an [issue](https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml). **Report a bug** under Updates & About in Settings (and in the phone app's About card) copies the diagnostics for you to paste in. Addresses, tokens and your home folder are stripped out of it.
- **Translate.** Both apps are in a few languages already and most of them are first drafts nobody who speaks the language has read yet. [Translating](docs/development/translating.md) shows how to fix texts right on GitHub without any setup and how to start a new language.
- **Ideas and questions** go in [Discussions](https://github.com/LunarKittyy/Telescope/discussions).
- **Docs** that confused you are bugs too. A pull request that fixes a sentence is very welcome.

Security problems go through a private report instead, see [SECURITY.md](SECURITY.md).

## With code

The Android app is in `android/` (Kotlin, Camera2) and the desktop app in `rust/` (Rust, Slint). [Architecture](docs/development/architecture.md) introduces the code and links to the detailed references.

### Setup

- **Desktop:** a recent stable Rust from [rustup](https://rustup.rs). On Linux you also need FFmpeg's headers, `clang`, fontconfig and a few fonts. [Building](docs/development/building.md#run-the-desktop-app-from-source) has the package list.
- **Android:** JDK 21 and the Android SDK with `platform-tools`, `platforms;android-34` and `build-tools;34.0.0`.

To try a change on a real phone without touching your normal install:

```bash
(cd android && ./gradlew installDev)               # a separate dev app on the phone
(cd rust && cargo run -p telescope-app -- dev --keep)  # a dev desktop that pairs with it
```

[Building](docs/development/building.md) has the details and [Packaging and CI](docs/development/packaging.md) covers the workflows.

### Before you start

For anything bigger than a small fix, open an issue or comment on one first to agree on the idea before you spend time on it. A few things are decided already:

- Telescope is for Linux and Windows. macOS isn't planned.
- No workarounds for a single phone model or a small brand. A problem that hits most phones of a major brand is worth fixing.
- Optional plugins don't have to work with each other, as long as a clash fails gracefully and is documented.

### Checks

Run the checks for what you changed before opening a pull request:

- Desktop: `cargo nextest run --workspace --locked` and `cargo clippy --workspace --locked --all-targets -- -D warnings` in `rust/`
- Android: `./gradlew lintDebug testDebugUnitTest` in `android/`

Keep the diff to your change. Don't reformat lines you didn't touch and only write comments that explain a non-obvious why. If something users see changes, update `docs/` in the same pull request.

### Commits

One short line that says what the commit does, in the imperative, like "Fix the preview freezing after a Wi-Fi drop". No description under it and no prefixes like `fix:` or `feat(desktop):`. Someone scrolling the history should get the idea from that line alone. If you need more than a line to explain why, that goes in the pull request.

### Pull requests

Fill in the [template](.github/pull_request_template.md). The comments in it explain each part. Anything that doesn't help a reviewer understand or check the change can go.

### Texts in the app

New texts go through `strings.xml` on the phone and `tr()` on the desktop so they can be translated ([Translating](docs/development/translating.md) has the details). Write them the way the rest of the app does:

- Tooltips and descriptions say what you can do with a control, in the imperative ("Set the gain back to default", not "Puts the gain back").
- Statuses say what's true or what's needed ("Restart required", not "Waits for a restart").
- One sentence is usually enough. Explanations and edge cases go in `docs/`.
- British spelling (colour, centre).

### Using AI tools

You can use whatever tools you like but you're responsible for what you submit. Read it, test it and make it as short and clear as if you'd written it by hand. You should be able to answer questions about your change yourself. Pull requests that are clearly generated with no effort behind them get closed. (And if you can, please don't use Gemini for coding, it causes more harm than good.)

## Keeping docs fresh

Every doc starts with a hidden line naming the code it describes and the commit it was last checked against:

```
<!-- covers: rust/crates/link/src/pairing.rs android/app/src/main/kotlin/com/telescope/Pairing.kt; checked: 1cbb8644b -->
```

`python3 tools/docs_fresh.py` lists the docs whose covered code changed since then, with the commits. If you changed code a doc talks about, read that doc. Fix what's out of date, then run `python3 tools/docs_fresh.py --bump docs/the-doc.md` to move `checked` up to the current commit. Do that when the doc was already right too, to save the next person from re-reading it for nothing. Bump after committing the code because it uses the current commit, then commit the doc with its new line.

A new doc needs its line (`covers` can be empty if no code stands behind it). `python3 tools/docs_fresh.py --paths` checks the lines and that every file a doc mentions exists and CI runs it. The flagged list shows up in the pull request's job summary and never fails a pull request.

Telescope is [AGPL-3.0](LICENSE) and code contributions are under the same licence. The name, logo and artwork are covered by [BRANDING.md](BRANDING.md). Please talk to me before sending changes to those.
