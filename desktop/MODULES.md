# Telescope Desktop - Module Reference

Quick index of what lives where. Detailed behaviour is in the source; this is for navigation.

---

## Entry point

### `main.py`
Dependency check, Qt app setup, `apply_theme()`, single-instance guard, plugin registration, config restore, event loop.
Registration order: `SetupPlugin → ConnectionPlugin → CameraControlPlugin → StreamOutputPlugin → TransformsPlugin → PreviewPlugin → MonitoringPlugin`.
Calls `win.apply_saved_config()` **after** all plugins are registered so every plugin's `set_config()` is available.

---

## `telescope/` package

### `app.py`
**TelescopeWindow** - thin coordinator shell.
- Owns the header bar (logo, plugin header widgets, settings menu, Start button), the three-column body, the footer (stream status, live FPS), and the tray icon.
- Owns `EventBus` and `StreamWorker` lifecycle.
- `register_plugin(p)` - calls `p.setup()`, routes `p.create_panel()` into the region `p.panel_region` names, appends `p.create_header_widget()` to the header slot.
- `_refresh_layout(force=False)` - redistributes panels across the three physical columns for the current width. `three` (≥1300px): left rail | video stage | right rail. `two` (≥900px): rails merged on the left, stage on the right. `one`: a single column, stage first. Called on resize (only re-lays out when the mode actually changes) and forced on each registration.
- `_show_settings_menu()` - builds the header's gear menu fresh on each click from every plugin's `create_menu_actions()`.
- Re-exports `STATUS_COLORS` from `theme.py`; `APP_VERSION` is the string shown next to the wordmark.
- `apply_saved_config()` - call after all plugins are registered; restores config round-trip for each plugin.
- **Two-phase start.** `_start()` calls `conn.get_stream_info()` for the URL (validates ADB/v4l2, builds URL), then hands off to `_spawn_wake()` - a background `conn.ensure_phone_streaming()` that brings the phone's camera up so the desktop's Start button is the only one anyone presses. `_on_wake_done()` (Qt slot) either runs `_begin_stream(url, token)`, which is the old synchronous body (worker, ctrl, pipeline, `on_stream_start()`), or re-enables the button and shows the reason. A `_wake_id` generation counter drops any result that lands after the user hit Stop, switched device, or quit. `_spawn_wake()` is split out for the same reason as `ConnectionPlugin._spawn_pair_probe()`: tests make it synchronous.
- `_stop(remote_stop=True)` - tears down worker and ctrl; `on_stream_stop()` on each plugin (ConnectionPlugin unforwards ADB there); invalidates any in-flight wake; and takes the phone's camera down with it via `_stop_phone_async()`. `remote_stop=False` is for the internal stop/start pairs that are really a desktop-side reconnect (`reconnect_stream()`, `restart_vcam_canvas()`) - bouncing the phone there would cost seconds and a lens re-open for nothing.
- `_stop_phone_async()` / `_drain_phone_stops(timeout=2.0)` - the remote stop runs off the UI thread but is tracked rather than fire-and-forget, so the quit paths (`closeEvent`, `_tray_quit`) can give it a bounded moment to actually leave the machine.
- Implements the public **`HostServices`** contract plugins call on `host` (see `plugin.py`): `schedule_save()`, `save_now()`, `switch_device()`, `reconnect_stream()`, `send_notification()`, `is_streaming()`, `stop_stream()`, `update_stream_output()`, `restart_vcam_canvas()`. Plugins go through these public methods only - they never touch private (`_`-prefixed) window internals like `_worker`/`_stop()`.
- `send_notification(title, body)` - uses `notify-send` on Linux, tray balloon on Windows.
- `save_now()` - writes global plugin configs (connection, setup) and per-device configs (camera_control, stream_output, transforms, monitoring) under `devices[selected]`. `schedule_save()` is the debounced variant plugins call after a settings change.
- `switch_device(prev, new)` - saves `prev` device's per-device configs, restores `new` device's; called by `ConnectionPlugin._on_device_changed()`.
- `is_streaming()` / `stop_stream()` / `update_stream_output(width, height, fps)` - public stream controls; `stop_stream()` is a guarded no-op when idle, and `update_stream_output()` forwards only the values a caller passes (`None` width/height means pass-through).
- `_plugin(name)` - central typed lookup of a registered plugin by name (replaces scattered inline `next(p for p ...)` scans).
- `_apply_config(cfg)` - routes global plugin slices to `p.set_config()`, per-device slices for the selected device, then calls `conn.select_device()` to set the active device in the combo.
- Utility exports: `acquire_single_instance()`, `listen_for_raise()`.

### `theme.py`
The app's entire visual definition: palette constants (`BG`, `SURFACE`, `ACCENT`, `FILL`, the `OK`/`WARN`/`ERR`/`DIM` status set, `STATUS_COLORS`), a dark `QPalette`, and the stylesheet built from those tokens.
- `apply_theme(app)` - sets Fusion as the base style, installs the palette, applies the QSS. Called once from `main.py`.
- No image assets and no third-party theme: icons are painted by `create_vector_icon()`, and controls that need a custom look (segmented toggles, badges, the preview stage) are targeted by object name or property selector.

### `plugin.py`
**TelescopePlugin** base class + **HostServices** contract + **EventBus**.
- `HostServices` (typing.Protocol): the public surface a plugin may call on its `host` handle - `schedule_save`, `save_now`, `switch_device`, `reconnect_stream`, `send_notification`, `is_streaming`, `stop_stream`, `update_stream_output`, `restart_vcam_canvas`. Structural typing only (`TelescopeWindow` implements it without inheriting). Keeps plugins off private window internals.
- `UNCHANGED`: sentinel for `update_stream_output` so `None` can be passed as a real value (pass-through resolution) distinct from "leave as-is".
- `TelescopePlugin`: override `setup`, `create_panel`, `on_stream_start`, `on_stream_stop`, `on_phone_state`, `process_frame`, `get_config`, `set_config`.
  - `panel_region` (class attr): `"left"` / `"right"` / `"center"` - which region the host puts the panel in. A preference, not a guarantee: narrow windows merge regions.
  - `create_header_widget()` → a compact widget for the window header, or `None`.
  - `create_menu_actions()` → `QAction`s for the header's settings menu, or `[]`. Lets a dialogs-only plugin skip having a panel.
- `EventBus(QObject)`: signals - `frame_ready`, `stream_start_requested`, `stream_stop_requested`, `stream_started`, `stream_stopped`, `stream_connected`, `phone_state_updated`, `device_changed`.

### `stream.py`
**StreamWorker(QThread)** - video capture and virtual camera output.
- Reads the authenticated MJPEG stream via `telescope/mjpeg_reader.py`'s `MjpegReader` (bearer token in the request header), writes to `pyvirtualcam`.
- `frame_pipeline: list[Callable]` - each callable receives an RGB numpy array and returns one; applied in order after resize.
- `update_output(width, height, fps)` - hot-swap output resolution/FPS without stopping the worker.
- Emits `status(kind, msg)` for the footer: `"ok"`, `"warn"`, `"fps"`, `"idle"`.
- Auto-reconnects on stream drop (`RECONNECT_DELAY = 3s`).

### `config.py`
Load/save of `telescope_config.json` with versioned schema (current: v2) and per-section validation. No cross-version migration: an unsupported or malformed file is backed up (`.invalid-<timestamp>`) and replaced with defaults.

**v2 schema:**
```
{
  "version": 2,
  "selected_device": "Phone1",
  "plugin_configs": { "connection": {...} },   ← global (mode, port, device list)
  "devices": {
    "Phone1": {
      "plugin_configs": {                       ← per-device
        "camera_control": {...},
        "stream_output":  {...},
        "transforms":     {...},
        "monitoring":     {...}
      }
    }
  }
}
```

`DEVICE_LOCAL_PLUGINS` frozenset marks which plugin names are per-device. No cross-version migration: a config below `CONFIG_VERSION`, or one that's unparseable/malformed at the top level, is backed up (`.invalid-<timestamp>`) and replaced with defaults. A current-version config instead has each top-level section (`plugin_configs`, `devices`, `selected_device`) validated independently, so one malformed section resets to its default without discarding the rest.

### `phone_client.py`
**PhoneControlClient** - authenticated HTTP client for the phone app's `/v1/state` and `/v1/control` endpoints (bearer token on every request).
- `send(action, **kwargs)` - queues a control command, POSTed as JSON by a single background worker thread in the order it was queued. Requests sharing an `action` are coalesced to the latest value while still queued (camera switches are sent individually and in order instead); a burst of slider drags can't have an older response land after a newer one. Failures are silently dropped.
- `get_state()` - fetch current camera state dict (lenses, ISO, shutter, WB, focus, AE comp, NR/edge mode, battery, etc.).

### `ip_utils.py`
Qt-free address helpers shared by the pairing flow and the device panel.
- `PairingAddress(ip, interface, kind)` - one address the phone can try, `kind` being `"lan"` / `"tailscale"` / `"other"`.
- `get_pairing_addresses()` - enumerates real adapters through `ifaddr` (never a route probe, which follows a VPN's default route), drops loopback/link-local/IPv6 and container/VM-only adapters, and returns candidates ordered LAN → Tailscale → other.
- `classify_ip(ip)` / `is_virtual_interface(name)` / `looks_like_vpn_interface(name)` / `describe_address(addr)` - the pieces the above is built from; `describe_address` also formats the dialog's "waiting on" lines.
- `MAX_PAIRING_CANDIDATES = 8` - cap on what goes into the QR code.
- `rank_ip()` / `best_ip()` / `extract_ip()` / `valid_ipv4()` - used for the *phone's* reported addresses in the device panel, unrelated to desktop discovery.

### `session_client.py`
**PhoneSessionClient** - authenticated HTTP client for the phone's session port (8766), where `SessionServer` answers whether or not a stream is running. Qt-free, like `pairing.py` and `ip_utils.py`.
- `ping()` → `PingResult(status, streaming, busy, local_only)` - `status` keeps the panel's existing `paired`/`not_paired`/`unreachable` vocabulary and the exact status-code mapping the old probe used. The state fields are `None` against an app predating the JSON body; `knows_session` is the flag for that.
- `start()` / `stop()` → `SessionResult(ok, error, unsupported)` - `unsupported` is its own outcome, set on a `404`, so an old APK means "fall back to connect-only" rather than "error".
- Owns `PING_PORT = 8766`, `REQUEST_TIMEOUT`, `START_TIMEOUT`, `START_POLL_INTERVAL`.

### `pairing.py`
**PairingServer** - the Qt-free one-shot pairing handshake: bind a port, mint a nonce and bearer token, wait for the phone's `POST /pair/{nonce}` echoing the token back.
- `start(advertise=None)` → `PairingOffer(payload, port, nonce, token, candidates)`, or `None` when there's no usable address. `advertise` overrides discovery - the USB path passes the loopback candidate reached through `adb reverse`.
- `payload` is the version-2 QR JSON (`version`, `port`, `candidates[]`, `nonce`, `token`); `PAIRING_PROTOCOL_VERSION` is bumped in lockstep with the app's.
- `PairingResult(name, ips, token, source_ip)` - `source_ip` is where the POST came from, i.e. a phone address proven reachable from here; `ConnectionPlugin._on_device_paired()` pins it as the device's `active_ip`.
- `PAIRING_PORT = 8765`, falling back to a random free port if it's taken.

---

## `telescope/widgets/`

### `widgets/common.py`
Reusable Qt widgets and helpers used across multiple panels:
- `control_row(label, widget, label_width, stretch)` / `control_row_widget(...)` - the standard settings row (right-aligned dim label, then the control) and its hideable variant. Imported as `_row` / `_row_widget` by the panels that use them heavily.
- `make_segmented(*buttons)` / `segmented_row(*buttons)` - style a run of radios/checkboxes as one joined pill strip. Sets `segmented` + `segPos` properties the stylesheet reads; the widgets stay ordinary, so `QButtonGroup` exclusivity and existing signals are untouched.
- `stretch_slider(slider, minimum)` - give a slider a minimum width and an Expanding policy so it fills its column instead of being pinned to a fixed track width.
- `NoScrollComboBox`, `NoScrollSlider`, `NoScrollSpinBox`, `NoScrollDoubleSpinBox` - scroll-wheel suppressed variants.
- `LogSliderRow` - slider + spinbox with logarithmic scaling (ISO, shutter speed).
- `PanSliderRow` - bipolar slider (−1 … +1) with centre-reset button.
- `create_separator()` - thin `QFrame` horizontal rule.
- `create_vector_icon(name, color)` - paints an icon to a `QIcon` with `QPainter`, tinted per use. Set: `connection`, `camera`, `stream`, `gear`, `status`, `qr`, `usb`, `transforms`, `logo`, `play`, `stop`, `expand`, `reset`.
- `ns_to_display(ns)`, `quality_label(q)` - display format helpers.

Note: white balance (Kelvin + tint sliders) is built directly in `plugins/camera_control.py` rather than through a shared widget - there is no `WbSliderRow` and no preset snapping.

### `widgets/lens_panel.py`
**LensPanel** - horizontal list of lens buttons populated from the phone's `/cameras` response. Emits `lens_selected(dict)` when the user switches lenses.

---

## `telescope/platform/`

### `platform/__init__.py`
Cross-platform constants and helpers: `IS_LINUX`, `IS_WINDOWS`, `adb_available()`, `adb_forward(port)`, `adb_unforward(port)`, `adb_exe()`, `bundled_apk_path()`, `_run(cmd)`.

### `platform/linux.py`
v4l2loopback helpers: `v4l2_load()`, `v4l2_module_loaded()`, `v4l2_devices_ready()`. Device constants: `V4L2_PHONE_DEV = /dev/video11`, `V4L2_OBS_DEV = /dev/video10`.

### `platform/windows.py`
UnityCapture helpers: `uc_is_registered()`, `unitycapture_dir()`, `download_unitycapture()`, `register_unitycapture()`.

---

## `telescope/plugins/`

### `plugins/connection.py`
**ConnectionPlugin** - mode selection, device list, port, ADB lifecycle. Registered first.
- UI: Wi-Fi/USB segmented toggle, pairing status + Pair Device button, IP combo, port field.
- `create_header_widget()` returns the device picker (combo + manage-devices gear) for the window header. Built eagerly in `create_panel()` via `_build_device_picker()`, so a host that never asks for a header still gets a working plugin - the combo is moved there, never duplicated.
- `_set_wifi_rows_visible(v)` - flips the header picker and the panel's address row together, since both are Wi-Fi-only.
- `get_stream_info()` → `(url, token, ok)` - validates port, checks v4l2loopback (Linux), ADB-forwards if USB mode; called by `app.py._start()`. Shows error dialogs on failure.
- `session_channel(token=None, usb=None)` - context manager yielding `(PhoneSessionClient, unavailable_status)` for the phone's port 8766. Wi-Fi hits the device IP directly; USB sets up a short-lived `adb forward` dedicated to that port and tears it down on exit. The single path used by the pair-status probe *and* the remote start/stop, so the two can't drift.
- `ensure_phone_streaming()` → `(ok, reason)` - brings the phone's camera up if it isn't already, then polls `/v1/ping` until it reports streaming (12s budget). Returns early when already streaming, and passes through (`True`) when the phone is too old to know `/v1/session`, so an APK mismatch degrades to connect-only rather than blocking. `reason` is display text. **Blocking - background thread only.**
- `stop_phone_streaming()` - best-effort `POST /v1/session {"action":"stop"}`. **Blocking - background thread only.**
- `on_stream_stop()` - unforwards ADB if a forward was established this session.
- `_DeviceDialog` / `_DeviceManagerDialog` (module-private) - device editing and the device-list manager behind the header's gear button; pairing is the only way to add a usable device.
- Config keys: `mode`, `port`, `devices_list` (no `selected_device` - it lives at config top-level).
- `select_device(name)` - called by host after `set_config()` to set the combo selection without triggering device-change logic.
- `DEFAULT_PORT = 8080` defined here.

### `plugins/camera_control.py`
**CameraControlPlugin** - lens selection, exposure, white balance, focus, OIS, and image tuning. `panel_region = "right"`.
- UI: `LensPanel` (horizontal lens buttons), camera capability info label, Exposure auto/manual + ISO + shutter sliders + exposure-compensation slider, White Balance auto/manual + Kelvin/tint sliders, OIS checkbox, Focus auto/manual + distance slider, noise-reduction and sharpening (edge mode) combos, black-level-lock checkbox, torch button.
- `derive_camera_control_view(state)` - pure function mapping a raw phone-state dict to a `CameraControlView` dataclass, independently testable without a `QApplication`.
- `on_stream_start`: stores ctrl, sets "Loading lenses..." placeholder, re-pushes the desktop's already-restored widget state to the phone via `_push_settings_to_phone()` (the phone otherwise keeps whatever it booted with until a control is touched).
- `on_phone_state(state)`: loads cameras into `LensPanel`, syncs exposure/WB/focus/OIS/AE-comp/NR/edge/black-level-lock/torch from phone state. Empty `state` dict (fetch failure) shows "Unavailable" on lens panel.
- `on_stream_stop`: clears lens panel and info label.
- `_update_camera_caps()`: disables manual exposure, manual WB, manual focus, or torch controls when the selected lens doesn't report support for them.
- Config keys: `exp_manual`, `iso`, `shutter_ns`, `ois`, `focus_manual`, `focus_diopters`, `wb_manual`, `wb_kelvin`, `wb_tint`, `ae_comp`, `nr_mode`, `edge_mode`, `bll`.

### `plugins/stream_output.py`
**StreamOutputPlugin** - output resolution, frame rate, and encoding settings.
- UI: resolution combo (pass-through / 1080p / 720p / 480p / 360p), playback FPS spinbox, JPEG quality slider, phone FPS spinbox.
- `get_stream_params()` → `(width, height, fps)` - called by `app.py._start()` to construct `StreamWorker`.
- `on_stream_start`: stores ctrl, schedules `_push_initial_settings` via `QTimer.singleShot(1500)` to sync quality/fps to the phone after connect.
- Resolution and FPS changes go through the public `host.update_stream_output()` for hot-swap without stream restart.
- Config keys: `resolution`, `fps`, `jpeg_quality`, `phone_fps`.

### `plugins/preview.py`
**PreviewPlugin** - the centre video stage and its pop-out. `panel_region = "center"`.
- UI: a letterboxed frame surface with a "LIVE" badge and a resolution badge floating over it (positioned, not laid out, so they never shift the picture), and a toolbar below with the "Hide"/"Show" toggle and "Pop out".
- Active by default - it's the centre of the window, not an opt-in card. The toggle remains as an escape hatch for anyone who'd rather not spend the decode.
- `process_frame(frame)` - runs on the stream reader thread; records the pre-downscale size for the resolution badge, downscales to `_CARD_MAX_W` for the in-window view (full resolution for the pop-out), emits a cross-thread Qt signal rather than touching any `QWidget` directly, then returns the frame unmodified (preview-only, doesn't alter the pipeline).
- Pop-out window auto-hides the in-card preview when opened, and closes/restores state when the main window is hidden (tray minimize).
- No config keys - preview visibility isn't persisted across restarts.

### `plugins/transforms.py`
**TransformsPlugin** - software frame transforms applied in the stream pipeline. `panel_region = "right"`.
- UI: flip (H/V segmented), rotation (None / 90 CW / 180 / 90 CCW), zoom slider (1×-5×), pan X/Y (enabled only when zoomed), and a "Reset transforms" button that drives the widgets so the handlers do the rest.
- `process_frame(frame)` - applies zoom crop then flip/rotate; runs on the worker thread. Reads plain Python attrs (`flip_h`, `flip_v`, `rotation`, `zoom`, `pan_x`, `pan_y`) written by the Qt thread; GIL makes these reads atomic.
- Config keys: `flip_h`, `flip_v`, `rotation`, `zoom`, `pan_x`, `pan_y`.

### `plugins/monitoring.py`
**MonitoringPlugin** - battery/temperature display and alerts.
- UI: live battery % + temp display, alert threshold spinboxes (battery %, temp °C).
- Subscribes to `bus.phone_state_updated`; also polls independently every 15 s via a daemon thread + `_Signals` inner class for thread-safe emit.
- Calls `host.send_notification()` for battery-low and overheating alerts (once per threshold crossing).
- Config keys: `battery_alert`, `temp_alert`.

### `plugins/setup.py`
**SetupPlugin** - entry points into **SetupDialog** and the Quick Start Guide.
- No panel: `create_panel()` returns `None` and `create_menu_actions()` contributes "Setup Drivers & APK…" and "Quick Start Guide…" to the header's settings menu. Neither is something you adjust mid-stream, so neither earns a rail slot.
- No stream lifecycle hooks.
- `SetupDialog` handles: v4l2loopback status/load (Linux), UnityCapture install (Windows), ADB status (Windows), APK install via ADB.
