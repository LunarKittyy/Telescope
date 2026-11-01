# plugin-host

Plugins people add, behind core's `Plugin` trait. A plugin is a folder with a `plugin.json`, a `main.lua`, usually a `card.slint` and a shader or two. [rust/docs/writing-plugins.md](../../docs/writing-plugins.md) is the guide for plugin authors and [rust/docs/plugin-wasm.md](../../docs/plugin-wasm.md) shows how a WASM runtime fits the same boundary later.

| File | What's in it |
|---|---|
| `guest.rs` | The runtime-neutral boundary: `Guest`, `HostCalls`, `HOST_API`, `HOOKS` and `GuestPlugin`, which turns any guest into a core `Plugin` |
| `lua.rs` | The Lua 5.4 runtime, one sandboxed state per plugin |
| `package.rs`, `manifest.rs` | Finding plugins, `plugin.json`, fingerprints, installing from a `.zip`, removing |
| `switches.rs` | The `plugins` section of the settings file (`off`, `noticed`, `added`, `on`, the update switches) |
| `updates.rs`, `updater.rs` | Checking a plugin's GitHub releases and auto-update |
| `i18n.rs`, `texts.rs` | A plugin's own `locale/<lang>.po`, and `plugin-texts` for keeping them up to date |
| `load.rs` | Package to running plugin, and the `factory` the binary hands the shell as `Services::plugins` |
| `testing.rs` | `PluginTest`, `check()` behind `check-plugin`, and `validate_shader` |

The examples in `example-plugins/` are tested end to end in `tests/lua.rs`. `tests/plugin_api.txt` freezes what plugins may use (run with `UPDATE_PLUGIN_API=1` after adding something on purpose).

## Writing a shader effect (for the Phase 2 effect sessions)

Every effect that changes pixels (Fun pack, chroma key, Desk mode's warp, the scopes' tints) becomes a `FrameStep::Shader`. The contract is in [`core/src/shader.rs`](../core/src/shader.rs) and it's the same whether the effect is a built-in Rust plugin or a Lua plugin someone adds. Media's executor runs it on the GPU with no plugin code involved per frame.

The shortest path to a working effect:

1. **Write the shader.** One function, `fn effect(uv: vec2<f32>) -> vec4<f32>`, returning the colour at `uv` (0..1, top left is `(0, 0)`). `sample(uv)` reads the picture so far, `frame.size`, `frame.time` and `frame.number` are there for effects that depend on size or move, and settings go in `struct Params { ... }` read as `params.x`. Colours are the camera's own gamma-encoded 0..1 values and the alpha you return is dropped. Look at `example-plugins/black_and_white/shaders/grey.wgsl`.
2. **Hand it over from the plugin.** In a built-in plugin, include the file and return it from `frame_step()`:
   ```rust
   fn frame_step(&mut self) -> Option<FrameStep> {
       self.on.then(|| FrameStep::Shader(ShaderStep {
           wgsl: include_str!("mirror.wgsl").into(),
           uniforms: json!({"amount": self.amount}),
           textures: Vec::new(),
       }))
   }
   ```
   Call `host.refresh_frame_step()` whenever a setting changes and the shell asks again. `PluginInfo::frame_stage` decides where the step runs (`effects` by default, `background` for chroma key, `geometry` for warps, `overlay` for things drawn on top). A Lua plugin returns `{ shader = "mirror.wgsl", uniforms = { amount = 0.5 } }` from `plugin.frame_step()` instead.
3. **Check that it compiles** in a unit test, without a GPU: `telescope_plugin_host::testing::validate_shader(&step, "mirror.wgsl")` assembles it the way media will and validates it with naga, with errors pointing at the shader's own lines. For a Lua plugin `cargo run -p telescope-plugin-host --bin check-plugin -- path/to/plugin` does that and more.
4. **Compare it with Python on the same frames.** `Media::pipeline()` gives a `FramePipeline`. `set_steps(vec![StagedStep { owner, stage, step }])` and then `process(frame, &ctx)` runs your step on a real I420 frame on the GPU, which is how the plugin-host session checked the example turning a coloured frame grey. Only the binary may depend on media, which puts a test like that where both are available (the `app` crate, or media's own tests with the WGSL inlined).

Things that trip you up:

- Params fields can be `f32`, `i32`, `u32`, `vec2/3/4<f32>` and `mat4x4<f32>`. A missing uniform is 0, a bool arrives as 0 or 1, a vector as a JSON array and a matrix as 16 numbers column by column. `pack_uniforms` refuses anything else.
- Don't declare `@group`/`@binding` yourself. Extra pictures (a LUT, a logo, a mask) go in `ShaderStep::textures` as `(name, Frame)` and arrive as `var <name>: texture_2d<f32>` in list order. Read them with `sample_texture(<name>, uv)`.
- `person_mask(uv)` gives how sure the vision service is that a person is at `uv`, 0..1, and `has_person_mask()` says whether there's a mask at all (`person_mask` is 0 everywhere while there isn't). Calling either is all it takes: a mask is only worked out for a shader that does, and it's uploaded only when it changes. The mask belongs to the picture as it reaches your step, so a mask shader that isn't first on the card sends the picture down once for the model. `core::shader` has the contract.
- Colour changes that only map colours to colours (grades, LUTs) belong in the colour pipeline through `host.set_colour_stage()`, not in a shader. They get baked into one LUT per stream with every other plugin's.
- Lua plugins can't hand over textures yet (frames aren't plain data a script should carry). An effect that needs a LUT or a mask image is a built-in for now.
- A step that fails to compile is logged once and frames pass through untouched. When an effect seems to do nothing, check the log or run `validate_shader` first.
