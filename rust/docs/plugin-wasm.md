# WASM plugins (design)

Lua comes first because it's the easiest way in for someone who has never written a plugin. WASM comes later for people who want to write plugins in Rust, C, Zig or anything else that compiles to it. This page shows that a WASM runtime fits the boundary the Lua one already uses, with no change to `core`, to the host API or to how a plugin is packaged. Nothing here is built yet.

## The boundary both runtimes use

Everything Lua-specific lives in `crates/plugin-host/src/lua.rs`. The rest of the plugin host only knows about the `Guest` trait in `guest.rs`:

```rust
pub trait Guest: Send {
    fn call(&mut self, calls: &mut HostCalls<'_>, hook: &str, args: Vec<Value>) -> Result<Value, String>;
    fn defines(&self, hook: &str) -> bool;
    fn listens(&self, event: &str) -> bool;
}
```

A hook is a name from `HOOKS` with its arguments as a JSON array and its answer as JSON. Events arrive as the `on_event` hook with the event serialised the way `core::Event` serialises it. The only way back into Telescope is `HostCalls::call(method, args)`, with a method from `HOST_API` and its arguments as a JSON object by name. `HostCalls` checks the plugin's permissions and queues the calls a hook without a host may still make. A runtime doesn't need to know about either.

`GuestPlugin<G: Guest>` turns any guest into a `core::Plugin`. It resolves the card's `.slint` file inside the plugin's folder, checks a frame step against `core::shader` and turns failures into log lines. None of that is repeated per runtime. The package format, install, update, switches and translations (`package.rs`, `updates.rs`, `switches.rs`, `i18n.rs`) never look inside the code at all, apart from `plugin.json`'s `main`.

## What a WASM host would add

A `WasmGuest` in a new `wasm.rs`, about the size of `lua.rs`, using wasmtime or wasmi:

- **Loading.** `plugin.json` names the module with `"main": "plugin.wasm"`. `Manifest::main()` learns to accept `.wasm` next to `.lua` and `load.rs` picks the runtime by extension. Compiled code is refused today (`package::COMPILED`). `.wasm` would be the one exception because a module can only do what its imports allow.
- **Calling hooks.** The module exports `telescope_call(hook_ptr, hook_len, args_ptr, args_len) -> u64` (a pointer and length to the JSON answer, packed into one value), plus `telescope_alloc` for the host to write strings into its memory. `defines` and `listens` read an exported list of hook and event names once at load instead of calling into the module to find out.
- **Calling the host.** The module imports `telescope.call(method_ptr, method_len, args_ptr, args_len) -> u64`, which goes straight to `HostCalls::call` for anything in `HOST_API`. A second import, `telescope.local`, covers what Lua's bridge handles without the host (`tr`, `trn`, reading the plugin's own files, logging). Those answers come from `PluginContext` through `HostCalls::context()`, so the two runtimes share that code too.
- **The sandbox.** No WASI filesystem, sockets or clock beyond a monotonic one. Fuel or epoch interruption stands in for Lua's instruction hook with the same 3 second limit per hook. A memory limit on the store stands in for Lua's 64 MB.
- **A small guest library** per language (a Rust crate first) wrapping the JSON calls in typed functions. That's the only new thing plugin authors see.

Shaders, cards and `plugin.json` stay exactly as they are for Lua. A WASM plugin hands over its frame step as the same `{ shader, uniforms }` table. Moving a plugin from Lua to WASM means rewriting `main.lua` and nothing else.

## Keeping the boundary clean

While Lua is the only runtime these rules keep the WASM path open.

- Nothing in `guest.rs`, `GuestPlugin` or `HOST_API` mentions Lua or takes a Lua type. Arguments and answers are `serde_json::Value`.
- A new host method goes into `HOST_API` with its arguments named, never as a Lua-only global. Lua's `host` table is built from `HOST_API` at load and picks up the new method without a change.
- Plugin-local services (translations, the plugin's own files) live on `PluginContext`, which every runtime reaches through `HostCalls::context()`.
- Hooks never hand a guest anything it could hold on to, like a handle or a callback. Requests that finish later come back as `request_done` events.
- `tests/plugin_api.txt` freezes the hooks, host methods, events, permissions and shader names. A WASM host has to pass the same list.
