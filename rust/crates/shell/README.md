# The shell

The window, the settings window and the plugins that belong to the window (`src/plugins/`). Everything from other crates comes in through `core`: `Services` for the link, decoding and cameras, `Plugin` for the cards.

## How the window is built

At startup `window::build` registers the plugins into `shell::State` (settings, device profiles, banners and folds, no Slint in it), then writes the window as one `.slint` document (`doc::window`) and compiles it with `slint-interpreter` (`compile.rs`).

- Every plugin's card is an element of that document. Cards don't move to another parent when the window crosses 1300 or 900 px. Each one works out its column, `y` and width from the window's `mode`, and each column scrolls on its own like Qt's three scroll areas.
- Every property and callback a card's component declares (read off its source by `source::scan`) is aliased on the window as `p<slot>-<name>`. That's how `host.set_ui_props()` reaches a card by name and how its callbacks come back as `UiEvent`s. Header widgets are `h<slot>-…` and settings sections `s<n>-…`.
- `@tr("…")` texts are translated before compiling (`source::translate`), so translations need nothing from Slint's gettext. Texts with named blanks (`{name}`) are filled in on the Rust side.
- Folding goes through the kit's `Folds` global. A card gets its key (`<plugin>/<title in English>`) from the window, and a `Section` gets its key from `scope: root.fold-key` plus its `key`.
- `settings.rs` builds the settings window the same way, two seconds after start while the app is idle (`prepare`), so the first click only shows it; it builds on that click if it's earlier. Its search walks the window's items when a search starts: elements with `accessible-id: "setting"` or `"card"` are what it finds, and the kit's `keywords` property puts extra words on them. `ElementHandle` can't do this, it leaves out whatever is scrolled off the page.

## How streams run

- `stream_flow.rs` is `stream_flow.py`: starting, stopping, waking and moving the panels between streams, with the bookkeeping in `session.rs`. It reaches the window only through `FlowHost`, so its tests use a stand-in.
- `streams.rs` is the window's side (app.py's `_FlowHost`, `_SourceHost` and the worker's signals). It keeps a `Source` per id, starts workers and updates the footer and tiles. Phones' threads report back to it on the UI thread.
- `sources.rs` holds the two kinds of source. A phone wakes through link's `check`, `open_route` and `ensure_streaming`, and looks for another route when its stream drops. When Connection's USB watch asks for a hand-over (`hand_over_stream`), the host gets the new route from link's `prepare_route`, the worker tries it next to the running one, and the phone then calls `commit_route` or `drop_route`. A plugin's `StreamSource` opens its own feed.
- `worker.rs` is `StreamWorker`. It reads the feed, decodes it through media's frame reader (H.264) or two JPEG decoders side by side, runs the plugins' frame steps and feeds the virtual camera. It reconnects, hands over to another route without a gap, settles after camera restarts and reports the same status texts as Python.
- `preview.rs` and `plugins/preview.rs` are the Preview card. The plugin keeps what the stage says and the toolbar; a new frame in the live worker's slot wakes the window (`WakingSlot` in `streams.rs`, one wake waiting at a time so a busy window drops frames and never queues them), which paints the newest at the stage's size, or the pop-out's. A 100 ms timer covers focus and the tiles. With a GPU painter from the app (`preview::use_gpu_painter`, media's scaler in `app/src/main.rs`), the window opens one wgpu device, hands it to Slint (`WGPUConfiguration::Manual`) and the painter draws into textures Slint shows as they are, so every window can show them and core never names wgpu. Without one, or with `TELESCOPE_NO_GPU=1`, Slint keeps its default renderer and media's swscale scaler fills an RGB image.
- `flow.rs` is the prototype's single stream, still used by `telescope-rs stream` until the app moves to `worker.rs`.

## Why interpreted

Measured with `card-startup-bench` (a throwaway crate: 30 renamed copies of `ui/cards/camera.slint` in one window, compiled with `slint-build` or interpreted), release build, median of 5 on an 8-core Ryzen:

| | First painted window |
|---|---|
| Compiled | 56 ms |
| Interpreted | 212 ms (compile 120, create 45, paint 47) |
| Python app | about 1070 ms |

## Drawing scenes

```bash
cargo run -p telescope-app -- scenes OUT [ids]     # from rust/; --list shows the ids, --scale 1 1.5 picks the scales
```

`telescope-rs scenes` (`app/src/scenes.rs`) builds the real window offscreen for every card, dialog and Settings page and writes each as `OUT/<scale>x/<id>.png`. Each scene has a stand-in link that answers the phone check and pairing as the scene needs and a virtual camera check that never answers. `scenes.rs` here does the drawing: a fresh profile per scene, waiting until plugins' threads have come back, then growing the window to fit its columns. A dialog scene opens its popup through the card's own callback (`Window::call_ui`) and cuts it out of the window (`Page::Dialog`).

The pictures are for looking at a card in every state it has, with real texts and layout, without a phone. `shell-scenes OUT` draws a few window states on their own, for checking the shell's layout.

### Adding a card's scenes

1. Add a `Scene::new("<id>", || Setup { ... })` to `all()` in `app/src/scenes.rs`. `Setup` holds the services (give `SceneLink` the answer the phone should give), `telescope_config.json` as the window should find it (`main_window_config(true)` is a paired Pixel 8), the width (`WIDTH_THREE_COL` for `picture()`), the starting height (700 for `picture()`, 900 for `grow(win, ..., 900)`), `Page::Settings(tab)` for a settings page and `then` for anything to do once the window has settled.
2. If the plugin needs something only the app sees (another crate's stand-in), add it next to `SceneLink`.
3. A card on its own (a plugin's panel 480 px wide with 16 px around it): `Page::Card { plugin }` with `CARD_WIDTH`. The window hides every other card and the card is cut out with its margin. `mic-off` is the example.
4. A popup: `Page::Dialog { frame, owner }` with the type names of the component that frames it and the one that opens it, and `then` calling the callback that opens it.

### Slint traps for card sessions

- A wrapped `Text` inside an `if` is measured as one line, so the card lays out too short and the rest overlaps it. Keep the element and use `visible: …` with `height: visible ? self.preferred-height : 0px` instead.
- Font sizes have to be whole pixels, the ones Qt draws at (the point size times 4/3, rounded): a fractional size lays every line out a pixel off Qt's. `Theme` has them.
- Wrapped text drifts a pixel every few lines unless the `Text` has `line-height-factor: Theme.line-factor(self.font-size)`. Qt rounds the line pitch to whole pixels (17px for 13px Plex, where Slint's own line is 16.9px).
- A card that waits for a phone (Python's `dim_until_paired`) is faded to 0.4 and disabled as well. Give `Card` `enabled: false` and its controls `enabled`/`disabled` too, so they take Qt's disabled colours.
- A card's `out` properties are readable from the window (`p<slot>-<name>`), but only `in` and `in-out` ones can be set.
