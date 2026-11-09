# Writing a plugin

Plugins are written in Lua, with GPU shaders for anything that changes the picture. To switch plugins on and off or install ones other people made, see [Plugins](plugins.md). Plugins written in Python for the old Telescope (API 1) don't run any more.

- [Start from an example](#start-from-an-example)
- [Checking your plugin](#checking-your-plugin)
- [plugin.json](#pluginjson)
- [main.lua](#mainlua)
- [The card](#the-card)
- [Changing the picture](#changing-the-picture)
- [Events and the host](#events-and-the-host)
- [Translating your plugin](#translating-your-plugin)
- [Testing it](#testing-it)
- [Ground rules](#ground-rules)
- [Sharing it](#sharing-it)

A plugin is a folder. It needs a `plugin.json` describing it and a `main.lua` with its code. Most plugins also add a `card.slint` for their card in the window and a `.wgsl` shader or two. You don't need to know Rust or anything about how Telescope works inside to write one. If you've written a bit of Lua, JavaScript or Python before you'll recognise most of it.

## Start from an example

Two small plugins live in [`rust/crates/plugin-host/example-plugins`](../rust/crates/plugin-host/example-plugins):

- **black_and_white** has a card with a switch and a slider, saves both and turns the picture grey with a shader. Each phone remembers its own setting.
- **stream_alerts** has no card. It listens for a stream dropping and coming back and shows a desktop notification each time. It also adds a keyboard shortcut action.

Copy whichever is closer to what you want, rename the folder and change `name` in `plugin.json` to match. The folder name and `name` have to be the same.

## Checking your plugin

From `rust/` in a Telescope checkout:

```bash
cargo run -p telescope-plugin-host --bin check-plugin -- path/to/my_plugin
```

It loads your plugin outside Telescope with a stand-in for the window, builds its card, compiles its shaders the way the GPU will, saves and restores its settings and takes it through a stream starting, dropping and stopping. Every problem gets a line with the file and line number it came from. It also gives you a note about your `repo` and `version` when update checks can't follow them.

Telescope loads a plugin you're working on straight from its folder when it runs with a dev profile (`TELESCOPE_DEV_PROFILE` set to a throwaway folder) and `TELESCOPE_DEV_PLUGINS` pointing at your plugin's folder. Several folders go in one variable separated like `PATH`. A plugin loaded that way is always on and stays on when you change its files.

## plugin.json

```json
{
  "name": "black_and_white",
  "title": "Black and white",
  "version": "1.0",
  "author": "Telescope",
  "description": "Turns the picture black and white. An example to start your own plugin from.",
  "api": 2,
  "category": "look",
  "icon": "colour",
  "when_off": "Without it there's no Black and white card and the picture keeps its colours.",
  "device_local": true
}
```

- `name` is your plugin's id, made of lowercase letters, digits and `_` and starting with a letter. Your settings are saved under it. Don't change it once people use your plugin or their settings won't find their way back.
- `title`, `version`, `author` and `description` show on the Plugins tab. Write the description for someone who has never heard of your plugin.
- `api` is the plugin API you wrote it for, which is `2` (Python plugins were `1`). It only goes up for a change that would break existing plugins.
- `category` picks the group on the Plugins tab: `camera`, `look`, `vision` (People), `preview`, `on_screen`, `sound` or `streaming`. Anything else lands under Extras.
- `when_off` tells people what goes missing when they switch your plugin off.
- `repo` is an optional `https://` link to your plugin's source, shown next to the author. A GitHub address also lets Telescope tell people about new versions (see [Sharing it](#sharing-it)).
- `permissions` lists the parts of the host API your plugin needs that could surprise someone. See the table under [Events and the host](#events-and-the-host).

These are optional and work like the class attributes of a built-in plugin:

| Key | What it does |
|---|---|
| `icon` | The plugin's icon on the Plugins tab, one of the kit's icon names like `colour` or `bell` |
| `main` | The script to start from, if it isn't `main.lua` |
| `device_local` | Each phone gets its own copy of your settings |
| `in_presets` | Presets save your settings and bring them back through `apply_preset` |
| `frame_stage` | Where your shader runs: `geometry`, `background`, `effects` (the default), `colour`, `privacy`, `overlay` and others, in the order the frame goes through them |
| `follows_focus` | With several phones streaming, follow the one the window shows (the default) or `false` for the main one |
| `header_side` | Where a header widget goes, `left` or `right` |
| `needs_vision` | Switching your plugin on downloads the vision pack first |

## main.lua

`main.lua` runs once when Telescope starts and returns a table. Telescope calls the functions in that table at the right moments, using the same names the built-in plugins use. Leave out whatever you don't need.

```lua
local plugin = {}

local on = false

function plugin.get_config()
  return { on = on }
end

function plugin.set_config(cfg)
  on = cfg.on == true
end

return plugin
```

| Function | When it's called |
|---|---|
| `setup()` | Once, after every plugin is loaded |
| `on_plugins_ready()` | Every plugin is set up and the saved settings are in |
| `card()` | Your card in the window (see below) |
| `header_widget()`, `settings_sections()` | A small control in the header bar, or blocks of your own on a Settings tab |
| `on_ui(event)` | Something happened on your card: `event.ui`, `event.callback` and `event.args` |
| `actions()`, `run_action(id, params)` | Things people can bind keyboard shortcuts to, and doing one (return a short text to show, like `"Torch: on"`) |
| `tray_actions()`, `on_tray_action(id)` | Entries in the tray menu |
| `on_stream_start(stream)`, `on_stream_stop()` | A stream really started or ended. `stream.source_id` names it |
| `on_focus_gained(stream)`, `on_focus_lost()` | With several phones, the window moved to or away from a stream that keeps running. Without these you get `on_stream_start` and `on_stream_stop` |
| `on_phone_state(state)` | The phone sent its state, with its lenses, battery and so on |
| `frame_step()` | Your shader and its settings, or `nil` to leave the picture alone |
| `get_config()`, `set_config(cfg)` | Saving and loading your settings |
| `apply_preset(cfg)` | A preset is applied (calls `set_config` if you don't have it) |
| `diagnostics()` | A few `{ "Label", "value" }` pairs for Copy diagnostics, with no names, addresses or tokens |
| `shutdown()` | Telescope is quitting |

`get_config()` returns a plain table of what you want remembered and `set_config()` gets it back on the next start. It might also get a table saved by an older version of your plugin or edited by hand. Check each value the way the example checks `cfg.on == true`. Raising an error in `set_config()` turns those settings down and Telescope keeps the values that do load on their own, with your defaults for the rest. Call `host.schedule_save()` after something changes and Telescope saves a moment later along with everything else.

Lua's own `table`, `string`, `math`, `utf8` and `coroutine` libraries are there, along with `os.time`, `os.clock`, `os.date` and `os.difftime`. Your plugin can't open files, start programs or reach the network. Lua's own pattern matching can get stuck backtracking for hours, and nothing can stop it halfway. So `string.find`, `string.match`, `string.gmatch` and `string.gsub` use a copy of Lua 5.4's matcher that counts its work. It gives the same results as Lua's, until one call has used up about a third of a second of work. Then it stops with an error that says the pattern took too many steps. Only a pattern with a lot of `*`, `+`, `-` or `?` in it, or `%b` on a long text, gets there. Use a simpler pattern or cut the text down first. Ordinary patterns on ordinary texts never get near it. A `gsub` with a function calls it as usual, and the time that function takes counts towards the hook's own time limit. On top of Lua's own you get:

- `host`, Telescope's side of things (see [Events and the host](#events-and-the-host))
- `tr`, `trn` and `N_` for texts people read
- `require("name")`, which loads `name.lua` from your plugin's folder (`require("lib.colours")` loads `lib/colours.lua`)
- `files.read("path")` for reading a file of your own plugin as text
- `json.encode(value)` and `json.decode(text)`
- `log.info(...)`, `log.warn(...)`, `log.error(...)` and `log.debug(...)`. `print` goes to `log.info`

## The card

`card()` returns where your card's `.slint` file is, the component to build from it and the values it starts with:

```lua
function plugin.card()
  return {
    file = "card.slint",
    component = "BlackAndWhiteCard",
    region = "right",
    props = { on = on, strength = strength },
  }
end
```

`region` is the column the card goes in (`left`, `right` or `center`). The `.slint` file is written in [Slint](https://slint.dev/), the toolkit the window is built with. It can use the same widgets as Telescope's own cards:

```slint
import { Card, RailRow, Icons, Slider, SwitchTile } from "@ui-kit/ui-kit.slint";

export component BlackAndWhiteCard inherits Card {
    in-out property <bool> on;
    in-out property <float> strength: 100;
    callback toggled(bool);
    callback strength-moved(float);

    title: @tr("Black and white");
    icon: Icons.colour;

    SwitchTile {
        icon: Icons.colour;
        text: @tr("Black and white picture");
        hue: root.hue;
        checked <=> root.on;
        toggled(value) => { root.toggled(value); }
    }
    ...
}
```

Inherit `Card` and give it a `title` and an `icon` from the kit's `Icons`, the way the example does. Telescope sets the card's colour from your plugin's group and makes it foldable. A control's `label` is only what screen readers announce, so name a row with a `RailRow` (its `icon` and `label`, the label showing as the icon's tooltip) or use a `SwitchTile`. Each `callback` your component declares reaches `on_ui()` with the callback's name and its arguments, written with `_` instead of `-` (`strength-moved` arrives as `strength_moved`). To change what the card shows later, set its properties with `host.set_ui_props("card", { on = true })`.

`header_widget()` and each entry of `settings_sections()` work the same way. A settings section also takes an `id` (used as `event.ui` and in `set_ui_props`), a `tab` (`general`, `streaming`, `video`, `audio`, `devices`, `shortcuts`, `about` or a new one with a `tab_title`), `order` and `keywords` for the Settings search.

## Changing the picture

Plugins change the picture with a shader written in [WGSL](https://www.w3.org/TR/WGSL/), the GPU language that runs everywhere Telescope does. Your Lua code never touches pixels. It hands Telescope the shader and its values from `frame_step()` and calls `host.refresh_frame_step()` whenever they change:

```lua
function plugin.frame_step()
  if not on then
    return nil
  end
  return { shader = "shaders/grey.wgsl", uniforms = { strength = strength / 100 } }
end
```

The shader has one function, `effect`, which gets a position in the picture and returns the colour that goes there. `sample(uv)` reads the picture as it came in:

```wgsl
struct Params {
    strength: f32,
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    let grey = dot(c.rgb, vec3<f32>(0.299, 0.587, 0.114));
    return vec4<f32>(mix(c.rgb, vec3<f32>(grey), params.strength), c.a);
}
```

- `uv` goes from `(0, 0)` at the top left to `(1, 1)` at the bottom right. Sampling somewhere else than `uv` moves things around (a mirror is `sample(vec2<f32>(1.0 - uv.x, uv.y))`).
- Colours are 0 to 1 in the camera's own values. Whatever alpha you return is ignored.
- `struct Params` holds your settings. Each field gets the value of the same name from `uniforms` (a missing one is 0). Fields can be `f32`, `i32`, `u32`, `vec2`, `vec3` and `vec4` of `f32`, or a `mat4x4<f32>`. Give a vector as a list, like `tint = { 1, 0.5, 0 }`, a matrix as 16 numbers column by column, and a `true` or `false` arrives as 1 or 0.
- `frame` has the picture's `size` in pixels, the stream's `time` in seconds and the frame's `number`, for effects that move.
- Don't declare bindings of your own. Telescope adds those around your code.

The shader runs on the GPU for every frame of every stream your plugin is on. Keeping up with 60 fps costs your Lua code nothing because it isn't involved per frame at all. `check-plugin` compiles every `.wgsl` file in your plugin and points at the line of any error.

## Events and the host

Things happening in Telescope reach your plugin through `plugin.events`, a table of functions named after the events you want:

```lua
local plugin = { events = {} }

function plugin.events.stream_lost(e)
  host.send_notification(tr("Stream dropped"), tr("Telescope is looking for a way back to the phone."))
end
```

Each function gets the event as a table with its fields (`e.source_id` here). The names and fields are the ones listed as `event` in [`tests/plugin_api.txt`](../rust/crates/plugin-host/tests/plugin_api.txt), from `stream_started` and `phone_state_updated` to `people_tracked` and `still_saved`. Your plugin can also send events of its own with `host.emit("my_plugin.something", { ... })` and listen for them under the same name, as long as the name starts with your plugin's name and a dot. The app's own events (`stream_stopped` and the like) are the app's to send, so `host.emit` refuses them.

`host` has everything else a plugin may ask of Telescope. The full list with each function's arguments is in [`tests/plugin_api.txt`](../rust/crates/plugin-host/tests/plugin_api.txt). A few you'll use most:

| Call | What it does |
|---|---|
| `host.schedule_save()` | Save your settings soon |
| `host.is_streaming()`, `host.focused_source_id()` | Whether a stream runs, and which one the window shows |
| `host.phone_control({ action = "torch", value = 1 })` | Send a control to the phone ([the control API](protocol.md)) |
| `host.show_issue(key, { title = "...", text = "..." })`, `host.clear_issue(key)` | A banner above the window's body |
| `host.view()`, `host.set_view({ zoom = 2 })` | The window's zoom, pan, flip and rotation |
| `host.take_view()`, `host.steer_view(zoom, pan_x, pan_y)`, `host.release_view()` | Moving the view yourself, like Auto framing does |
| `host.want_people(true)` | Hear `people_seen` and `people_tracked` events (needs the vision download) |
| `host.watch_motion({ { 0.6, 0, 1, 1 } }, 5)` | Hear `motion_seen` when something moves in those parts of the picture |
| `host.add_preview_layer({ shapes = { ... } })` | Draw on the preview only, never in the camera output |
| `host.want_sensors({ "orientation" })`, `host.sensor_reading()` | The phone's motion and light sensors |
| `host.schedule(1000, "tick")`, `host.cancel_schedule("tick")` | A timer: `plugin.events.timer_fired` gets `{ token = "tick" }` once after a second. Schedule it again from there to repeat |

Methods that need an owner (the view, preview layers, motion zones) use your plugin's title, which is also what the window shows when your plugin has the view.

Some calls need a permission in `plugin.json`. People see the permissions on the Plugins tab before they switch your plugin on.

| Permission | What it allows |
|---|---|
| `phone` | Changing the phone's camera settings and taking photos (`phone_control`, `take_still`) |
| `streams` | Starting and stopping streams and changing the virtual camera |
| `app` | Quitting or restarting Telescope, showing and hiding its window |
| `settings` | Reading other plugins' settings, applying presets and moving a phone's settings |
| `notifications` | Desktop notifications |

A call without its permission raises an error naming the one it needs. A banner button from `host.show_issue` is held to the same rule: one that starts a stream or turns the camera on needs `streams`, and a button that opens a link only takes an `http` or `https` address.

`card()`, `frame_step()`, `get_config()`, `set_config()` and the other functions that describe your plugin run without the host because the window can ask for them at any time. Asking the host something there raises an error, but `set_ui_props`, `refresh_frame_step`, `schedule_save`, `update_tray_action`, `emit`, `schedule` and `cancel_schedule` still work. They're sent once the next event or hook comes along. That's how the example updates its card from `set_config()`.

## Translating your plugin

Wrap every text people read in `tr()`:

```lua
tr("Strength")
tr("Blurring {name}", { name = phone_name })   -- {fields} instead of joining strings
trn("{n} face", "{n} faces", faces)            -- anything with a number in it
local TITLE = N_("My overlay")                 -- marked here, translated with tr(TITLE) where it shows
```

Each language is a `.po` file in a `locale` folder inside your plugin, like `locale/sv.po` for Swedish. From `rust/` in a Telescope checkout:

```bash
cargo run -p telescope-plugin-host --bin plugin-texts -- extract path/to/my_plugin     # finds your texts and writes locale/my_plugin.pot
cargo run -p telescope-plugin-host --bin plugin-texts -- new sv path/to/my_plugin      # starts locale/sv.po
```

Run `extract` again after changing a text. New texts are added to every `.po` file and translations still in use stay as they are. `status` shows how far each language is. A text is looked up in your plugin's catalog first, then in Telescope's own (common words like "Camera" arrive translated already) and then it stays as you wrote it. `sv_SE` uses `sv.po` when there's no `sv_SE.po`. Texts in your `.slint` file use Slint's `@tr()`. [Translating](translating.md) explains the `.po` format.

A `.po` file that can't be read never stops your plugin from loading. The plugin stays in English and the log names the file. `check-plugin` lists broken catalogs, along with translations that have a `{field}` the English doesn't (those show in English). Catalogs count as part of your plugin's files, so people have to switch it on again after you change one.

## Testing it

`check-plugin` covers the basics. For anything more specific the plugin host has a test harness in Rust, `telescope_plugin_host::testing::PluginTest`, which runs your plugin exactly the way Telescope does against a stand-in host that writes down every call:

```rust
use telescope_plugin_host::testing::PluginTest;
use telescope_core::Event;

#[test]
fn a_drop_shows_one_notification() {
    let mut t = PluginTest::open("path/to/stream_alerts").unwrap();
    t.start_stream("phone");
    t.event(Event::StreamLost { source_id: "phone".into() });
    t.event(Event::StreamLost { source_id: "phone".into() });
    assert_eq!(t.take_calls(), ["send_notification Stream dropped"]);
}
```

`t.ui("card", "toggled", vec![json!(true)])` presses something on your card, `t.config()` and `t.set_config(...)` save and load, `t.shader()` gives your current frame step and `testing::validate_shader` compiles it. `PluginTest::open_in(folder, "sv")` loads it in another language. The tests for the two examples in [`tests/lua.rs`](../rust/crates/plugin-host/tests/lua.rs) show the rest.

## Ground rules

- **Don't reach into other plugins.** Use events and the host. Other plugins can be switched off and yours has to work without them.
- **Keep your functions quick.** They run on the window's thread. One that takes longer than 3 seconds is stopped with an error. A plugin can use up to 64 MB of memory.
- **Don't count on the order of a table's keys.** Lua doesn't keep it and neither does `json.encode`.
- **Don't write files into your plugin's folder.** Telescope doesn't let your code do that anyway. If you or anyone else changes a file there by hand it counts as a new version, which stays off until it's switched on again.
- **Ship source code.** A plugin with compiled files in it (Lua bytecode, `.so`, `.dll` and the like) isn't loaded.

When something in your plugin raises an error Telescope logs it with the file and line and carries on. The other plugins still hear about everything. An error while loading shows on your plugin's tile on the Plugins tab.

## Sharing it

Run `check-plugin` one more time first. Then zip your plugin's folder with the `.zip` holding that one folder rather than the files inside it (leave out `.git` and the like):

```
black_and_white.zip
└── black_and_white/
    ├── plugin.json
    ├── main.lua
    ├── card.slint
    └── shaders/
        └── grey.wgsl
```

Bump `version` in `plugin.json` whenever you share a new one. People install it with **Install** on the Plugins tab, which replaces the old version and leaves its settings as they were. They have to switch your plugin on again after each update.

If `repo` points at GitHub you can publish each version as a GitHub Release with the `.zip` attached, tagged with the version number (`v1.3` or `1.3`, matching `"version": "1.3"`). Telescope compares the tag of your latest release with the installed `version` once a day and mentions a newer one on the Plugins tab. Name the file after your plugin (`black_and_white.zip`) when a release has more than one `.zip`. People who switched on **Install updates without asking** get your release installed and running from their next start without seeing it first. Treat your GitHub account like the keys to their computers.
