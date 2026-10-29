# The shell

The window, the settings window and the plugins that belong to the window (`src/plugins/`). Everything from other crates comes in through `core`: `Services` for the link, decoding and cameras, `Plugin` for the cards.

## How the window is built

At startup `window::build` registers the plugins into `shell::State` (settings, device profiles, banners and folds, no Slint in it), then writes the window as one `.slint` document (`doc::window`) and compiles it with `slint-interpreter` (`compile.rs`).

- Every plugin's card is an element of that document. Cards don't move to another parent when the window crosses 1300 or 900 px. Each one works out its column, `y` and width from the window's `mode`, and each column scrolls on its own like Qt's three scroll areas.
- Every property and callback a card's component declares (read off its source by `source::scan`) is aliased on the window as `p<slot>-<name>`. That's how `host.set_ui_props()` reaches a card by name and how its callbacks come back as `UiEvent`s. Header widgets are `h<slot>-…` and settings sections `s<n>-…`.
- `@tr("…")` texts are translated before compiling (`source::translate`), so translations need nothing from Slint's gettext. Texts with named blanks (`{name}`) are filled in on the Rust side.
- Folding goes through the kit's `Folds` global. A card gets its key (`<plugin>/<title in English>`) from the window, and a `Section` gets its key from `scope: root.fold-key` plus its `key`.
- `settings.rs` builds the settings window the same way, the first time it opens. Its search reads the window's accessible tree (`i-slint-backend-testing`'s `ElementHandle`): elements with `accessible-id: "setting"` or `"card"` are what it finds.

## How streams run

- `stream_flow.rs` is `stream_flow.py`: starting, stopping, waking and moving the panels between streams, with the bookkeeping in `session.rs`. It reaches the window only through `FlowHost`, so its tests use a stand-in.
- `streams.rs` is the window's side (app.py's `_FlowHost`, `_SourceHost` and the worker's signals). It keeps a `Source` per id, starts workers and updates the footer and tiles. Phones' threads report back to it on the UI thread.
- `sources.rs` holds the two kinds of source. A phone wakes through link's `check`, `open_route` and `ensure_streaming`, and looks for another route when its stream drops. A plugin's `StreamSource` opens its own feed.
- `worker.rs` is `StreamWorker`. It reads the feed, decodes it through media's frame reader (H.264) or two JPEG decoders side by side, runs the plugins' frame steps and feeds the virtual camera. It reconnects, hands over to another route without a gap, settles after camera restarts and reports the same status texts as Python.
- `flow.rs` is the prototype's single stream, still used by `telescope-rs stream` until the app moves to `worker.rs`.

## Why interpreted

Measured with `card-startup-bench` (a throwaway crate: 30 renamed copies of `ui/cards/camera.slint` in one window, compiled with `slint-build` or interpreted), release build, median of 5 on the 8-core Ryzen from `rust/FINDINGS.md`:

| | First painted window |
|---|---|
| Compiled | 56 ms |
| Interpreted | 212 ms (compile 120, create 45, paint 47) |
| Python app | about 1070 ms |

## Comparing with Qt

```bash
python parity/render_scenes.py OUT/qt                          # every Qt scene at 1x and 1.5x
cargo run -p telescope-app -- scenes OUT/rust                  # the scenes Rust can draw, same names (ids and --scale 1 1.5 optional)
python parity/compare_scenes.py OUT/qt OUT/rust OUT/review     # what differs, with a review page
```

`telescope-rs scenes` (`app/src/scenes.rs`) builds the real window offscreen for each scene, with a stand-in link that answers the phone check as the scene needs and a virtual camera check that never answers, the same way `desktop/scripts/translation_scenes*.py` patch the Python window. `scenes.rs` here does the drawing: a fresh profile per scene, waiting until plugins' threads have come back, then growing the window to its columns like `text_fit._grow_to_content`.

Today it draws `main-*`, Connection's card states (`connection-ready` and the rest, but the two that need Windows) and `settings-*`. None of them match yet. The Qt scenes show every built-in card and settings page, so the pictures differ until those are ported, even where the ported card itself looks the same.

### Adding a card's scenes

1. Find the scene in `desktop/scripts/translation_scenes*.py` and read what its builder patches and sets.
2. Add a `Scene::new("<same id>", || Setup { ... })` to `all()` in `app/src/scenes.rs`. `Setup` holds the services (give `SceneLink` the answer the phone should give), `telescope_config.json` as the window should find it (`main_window_config(true)` is `main_window`'s Pixel 8), the width (`WIDTH_THREE_COL` for `picture()`), the starting height (700 for `picture()`, 900 for `grow(win, ..., 900)`), `Page::Settings(tab)` for a settings page and `then` for anything to do once the window has settled.
3. If the plugin needs something only the app sees (another crate's stand-in), add it next to `SceneLink`.
4. Render it, look at it next to the Qt one in the review page and fix what differs in your card.
5. Once `compare_scenes.py` stops listing it, mark the scene `ported` in `rust/parity/ledger.toml` with `rust = "rust/app/src/scenes.rs"`. From then on the comparison fails if it drifts.

`shell-scenes` and `parity/qt_shell_scenes.py` are the older pair: a few window states rendered by both sides with only the ported plugins, for checking the shell's layout on its own (`parity/region_diff.py` compares them by region).
