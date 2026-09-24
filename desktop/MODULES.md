# Telescope Desktop - Module Reference

Quick navigation index; see source code for detailed behavior.

---

## Entry point

### `main.py`
Dependency check, Qt app setup, `apply_theme()`, single-instance guard, plugin registration, config restore, event loop.
Registration order: `SetupPlugin → ConnectionPlugin → CameraControlPlugin → StreamOutputPlugin → TransformsPlugin → PreviewPlugin → OnboardingPlugin → MonitoringPlugin`. Preview comes before Onboarding so the stage is listening when the checklist first says whether it needs the space.
Calls `win.apply_saved_config()` **after** all plugins are registered so every plugin's `set_config()` is available.

---

## `telescope/` package

### `app.py`
**TelescopeWindow** - thin coordinator shell.
- Owns the header bar (plugin header widgets on the left, settings menu and Start button on the right), the column body, the footer (stream status, FPS, throughput), and the tray icon.
- Owns `EventBus` and `StreamWorker` lifecycle.
- `register_plugin(p)` - calls setup, routes panel to the named region, appends header widget.
- `_refresh_layout(force=False)` - redistributes panels across columns by width (a panel its plugin has hidden stays hidden): `three` (≥1300px) left/center/right, `two` (≥900px) left+right/center, `one` center-first. Both rails are `_RAIL_WIDTH` wide so the preview sits centred. Triggered on resize (no-op if layout mode unchanged) and on registration.
- `_show_settings_menu()` - builds the header's settings menu fresh on each click from every plugin's `create_menu_actions()`.
- Re-exports `STATUS_COLORS` from `theme.py`.
- `apply_saved_config()` - call after all plugins are registered; restores config round-trip for each plugin.
- **Two-phase start.** `_start()` calls `conn.get_stream_info()` (validates ADB/v4l2, builds URL), snapshots `conn.session_target()` on the GUI thread, then `_spawn_wake()` (background thread bringing phone camera up). `_on_wake_done()` either runs `_begin_stream()` (worker, ctrl, pipeline) or re-enables Start and shows the error. `_wake_id` counter drops stale results after user Stop/device switch/quit. Split for testability (tests call synchronously).
- `_stop(remote_stop=True)` - tears down worker and ctrl; `on_stream_stop()` on each plugin (ConnectionPlugin releases its USB forward there); invalidates any in-flight wake; and takes the phone's camera down with it via `_stop_phone_async()`. `remote_stop=False` is for the internal stop/start pairs that are really a desktop-side reconnect (`reconnect_stream()`, `restart_vcam_canvas()`) - bouncing the phone there would cost seconds and a lens re-open for nothing.
- `_stop_phone_async()` / `_drain_phone_stops(timeout=2.0)` - the remote stop runs off the UI thread but is tracked rather than fire-and-forget, so the quit paths (`closeEvent`, `_tray_quit`) can give it a bounded moment to actually leave the machine. Both quit paths then call every plugin's `shutdown()`.
- Implements the public **`HostServices`** contract (see `plugin.py`): `schedule_save()`, `save_now()`, `switch_device()`, `forget_device_settings()`, `reconnect_stream()`, `send_notification()`, `is_streaming()`, `stop_stream()`, `update_stream_output()`, `restart_vcam_canvas()`. Plugins only call these; private internals (`_worker`, `_stop()`) are hidden.
- `send_notification(title, body, urgent=True)` - uses `notify-send` on Linux (critical urgency only when `urgent`), tray balloon on Windows.
- `save_now()` - writes global plugin configs (connection, setup) and per-device configs (camera_control, stream_output, transforms, monitoring) under `devices[selected]`. `schedule_save()` is the debounced variant plugins call after a settings change.
- `switch_device(prev, new)` - saves `prev` phone's per-device configs, restores `new` phone's; called by `ConnectionPlugin._activate_profile()`. Keys are phone ids.
- `forget_device_settings(id)` - deletes a removed phone's stored settings.
- `is_streaming()` / `stop_stream()` / `update_stream_output(width, height, fps)` - public stream controls; `stop_stream()` is a guarded no-op when idle, and `update_stream_output()` forwards only the values a caller passes (`None` width/height means pass-through).
- `_plugin(name)` - central typed lookup of a registered plugin by name (replaces scattered inline `next(p for p ...)` scans).
- Footer "Throughput" (Mbps) readout, colored amber on sustained real decode-rate struggle; live FPS/resolution readout reflects the frame's actual current shape rather than the frozen stream-start size.
- Pending-resolution tracking: `_on_resolution_pending()` marks changes in flight (8s timeout); readout amber until fps readout confirms new size or timeout fires error.
- `_start_reconnecting_animation()` / `_tick_reconnecting_animation()` - animates the "Stream dropped - reconnecting" footer status (yellow, cycling "." → ".." → "..." once a second) instead of a static line.
- `_apply_config(cfg)` - routes global plugin slices to `p.set_config()`, per-device slices for the selected device, then calls `conn.sync_active_profile()` so the restored selection doesn't spuriously re-trigger a profile switch.
- Utility exports: `acquire_single_instance()`, `listen_for_raise()`.

### `theme.py`
The app's entire visual definition: palette constants (`BG`, `SURFACE`, `ACCENT`, `FILL`, the `OK`/`WARN`/`ERR`/`DIM` status set, `STATUS_COLORS`), a dark `QPalette`, and the stylesheet built from those tokens.
- `apply_theme(app)` - sets Fusion as the base style, installs the palette, applies the QSS. A no-op once installed, since re-applying repolishes every live widget.
- No image files and no third-party theme: icons are inline SVG rendered by `create_vector_icon()`, and controls that need a custom look (segmented toggles, card actions, the preview stage) are targeted by object name or property selector.
- The layout rules every panel and dialog follows (type scale, control column, button roles) are written at the top of `widgets/common.py`.

### `plugin.py`
**TelescopePlugin** base class + **HostServices** contract + **EventBus**.
- `HostServices` (typing.Protocol): the public surface a plugin may call on its `host` handle - `schedule_save`, `save_now`, `switch_device`, `forget_device_settings`, `reconnect_stream`, `send_notification`, `is_streaming`, `stop_stream`, `update_stream_output`, `restart_vcam_canvas`. Structural typing only (`TelescopeWindow` implements it without inheriting). Keeps plugins off private window internals.
- `UNCHANGED`: sentinel for `update_stream_output` so `None` can be passed as a real value (pass-through resolution) distinct from "leave as-is".
- `TelescopePlugin`: override `setup`, `create_panel`, `on_stream_start`, `on_stream_stop`, `on_phone_state`, `process_frame`, `get_config`, `set_config`, `shutdown`.
  - `panel_region` (class attr): `"left"` / `"right"` / `"center"` - which region the host puts the panel in. A preference, not a guarantee: narrow windows merge regions.
  - `create_header_widget()` → a compact widget for the window header, or `None`.
  - `create_menu_actions()` → `QAction`s for the header's settings menu, or `[]`. Lets a dialogs-only plugin skip having a panel.
- `EventBus(QObject)`: signals - `stream_started`, `stream_stopped`, `stream_connected`, `phone_state_updated`, `device_changed`, `phones_changed(int)`, `add_phone_requested`, `setup_needed(bool)`, `camera_switched`, `resolution_change_requested`. `phones_changed`, `add_phone_requested` and `setup_needed` are how the checklist, Connection and Preview cooperate without calling each other.

### `stream.py`
**StreamWorker(QThread)** - video capture and virtual camera output.
- Reads the authenticated MJPEG stream via `telescope/mjpeg_reader.py`'s `MjpegReader` (bearer token in the request header), writes to `pyvirtualcam`.
- `frame_pipeline: list[Callable]` - each callable receives an RGB numpy array and returns one; applied in order after resize.
- `update_output(width, height, fps)` - hot-swap output resolution/FPS without stopping the worker. An FPS change reopens only the virtual camera (`_run_vcam()`); the phone connection and reader thread stay up.
- Emits `status(kind, msg)` for the footer: `"ok"`, `"warn"`, `"fps"`, `"net"`, `"net_warn"`, `"reconnecting"`, `"idle"`.
- Auto-reconnects on stream drop (`RECONNECT_DELAY = 3s`).

### `config.py`
Load/save of `telescope_config.json` with versioned schema (current: v3) and per-section validation.

**v3 schema:**
```
{
  "version": 3,
  "selected_device": "3f9c…",                  ← phone id
  "plugin_configs": {
    "connection": {                             ← global
      "computer_id": "8d1e…", "computer_name": "desk", "route": "auto",
      "phones": [{"id", "name", "token", "ips", "active_ip"}], "selected_phone": "3f9c…"
    },
    "setup": {...}
  },
  "devices": {
    "3f9c…": {
      "plugin_configs": {                       ← per phone
        "camera_control": {...},
        "stream_output":  {...},
        "transforms":     {...},
        "monitoring":     {...}
      }
    }
  }
}
```

`DEVICE_LOCAL_PLUGINS` frozenset marks which plugin names are per phone. `_upgrade_from_v2()` turns a v2 file into v3 by dropping the connection section, `devices` and `selected_device` (phones were keyed by name then) and keeping everything else. Anything older, or unparseable/malformed at the top level, is backed up (`.invalid-<timestamp>`) and replaced with defaults. A current config has each top-level section (`plugin_configs`, `devices`, `selected_device`) validated independently, so one malformed section resets to its default without discarding the rest.

### `phone_client.py`
**PhoneControlClient** - authenticated HTTP client for phone's `/v1/state` and `/v1/control` endpoints (bearer token on each request).
- `send(action, **kwargs)` - queues commands, coalesces repeated actions to latest value, sends in order via background thread. Camera switches always go individually; slider bursts stay ordered (no stale response overtakes newer ones). Failures silently dropped.
- `get_state()` - fetch current camera state dict (lenses, ISO, shutter, WB, focus, AE comp, NR/edge mode, battery, etc.).

### `ip_utils.py`
Qt-free address helpers for pairing and routing.
- `PairingAddress(ip, interface, kind)` - one address the phone can try, `kind` being `"lan"` / `"tailscale"` / `"other"`.
- `get_pairing_addresses()` - enumerates real adapters through `ifaddr` (never a route probe, which follows a VPN's default route), drops loopback/link-local/IPv6 and container/VM-only adapters, and returns candidates ordered LAN → Tailscale → other.
- `classify_ip(ip)` / `is_virtual_interface(name)` / `looks_like_vpn_interface(name)` - the pieces the above is built from.
- `MAX_PAIRING_CANDIDATES = 8` - cap on what goes into the QR code.
- `rank_ip()` / `valid_ipv4()` - used on the *phone's* reported addresses when choosing which to probe.

### `session_client.py`
**PhoneSessionClient** - HTTP client for the phone's session port (8766), where `SessionServer` answers whether or not a stream is running. Qt-free.
- `hello(timeout)` → `(phone_id, phone_name)` or `None` - unauthenticated; which phone is on the other end.
- `ping()` → `PingResult(status, streaming, busy, local_only, phone_id, phone_name)` - `status` is `paired` / `not_paired` (401) / `unreachable` (anything else, including a body that isn't Telescope's JSON).
- `start()` / `stop()` → `SessionResult(ok, error)`.
- `unpair()` - asks the phone to forget this computer (best effort).
- Owns `PING_PORT = 8766`, `REQUEST_TIMEOUT`, `START_TIMEOUT`, `START_POLL_INTERVAL`.

### `phones.py`
Qt-free model of paired phones and how to reach them; adb, the session client and discovery are injected, so it's tested without any of them.
- `Phone(id, name, token, ips, active_ip)` with `to_dict()` / `from_dict()`.
- `Route(kind, host, serial)`, `Resolution(status, route, usb_note, streaming, busy)`; statuses `READY`, `UNREACHABLE`, `NOT_PAIRED`, `LOCAL_ONLY`, `USB_NEEDS_ATTENTION`; USB reasons `USB_NO_ADB`, `USB_NO_CABLE`, `USB_UNAUTHORIZED`, `USB_OTHER_PHONE`, `USB_APP_CLOSED` (`usb_note_text()` words them for the UI).
- `RouteResolver.resolve(phone, preference)` - USB first (each usable adb device, verified by `/v1/hello` id, then `/v1/ping`), then Wi-Fi probed in parallel over mDNS-discovered, last-good and stored addresses. Blocking.
- `UsbTunnels` - refcounted `adb forward tcp:0` per (serial, remote port).
- `STREAM_PORT = 8080`, `ROUTE_AUTO` / `ROUTE_USB` / `ROUTE_WIFI`.

### `discovery.py`
**LanDiscovery** - browses `_telescope._tcp` with `zeroconf` and maps the TXT `id` to current IPv4 addresses. `lookup(phone_id)` never blocks; without zeroconf or multicast it returns nothing and stored addresses are used.

### `pairing.py`
**PairingServer** - the Qt-free one-shot pairing handshake: bind a port, mint a nonce and bearer token, wait for the phone's `POST /pair/{nonce}` echoing the token back.
- `PairingServer(on_paired, computer_id, computer_name)`; `start()` → `PairingOffer(payload, port, nonce, token, candidates, usb_payload)`. `candidates` may be empty (no network); `usb_payload` advertises only `127.0.0.1`, reached through `adb reverse`.
- `payload` is the version-3 JSON (`version`, `port`, `candidates[]`, `nonce`, `token`, `computer_id`, `computer_name`); `PAIRING_PROTOCOL_VERSION` moves in lockstep with the app's.
- `PairingResult(name, ips, token, source_ip, phone_id)` - a POST without `phone_id` is rejected. `source_ip` is where the POST came from, i.e. a phone address proven reachable from here, and becomes the phone's first `active_ip`.
- `PAIRING_PORT = 8765`, falling back to a random free port if it's taken.

---

## `telescope/widgets/`

### `widgets/common.py`
Reusable Qt widgets and helpers used across multiple panels:
- The layout rules (type scale, control column, segment and button widths, button roles) are written as a comment block at the top of this file; new UI should stay inside them.
- `control_row(label, widget, label_width, stretch)` / `control_row_widget(...)` - the standard settings row (dim label, then the control starting at the shared control column) and its hideable variant. Imported as `_row` / `_row_widget` by the panels that use them heavily.
- `SegmentButton` / `segmented_row(*buttons, fill=True)` - checkable buttons joined into one strip with equal segments: equal shares of the row when `fill`, `SEGMENT_WIDTH` each otherwise. Exclusivity comes from the `QButtonGroup` they're added to.
- `slider_row(slider, readout, gutter)` / `value_label()` - slider plus fixed-width readout, optionally reserving the spinbox column so tracks line up across a card.
- `card_layout()`, `add_card_header(..., action=)`, `card_action()`, `add_section_heading()` - card structure; the header carries the card's one action button.
- `action_button()`, `button_row()`, `dialog_layout()`, `dialog_header()`, `dialog_buttons()`, `wrapped_note()` / `WrapLabel` - dialog kit. `WrapLabel` measures its text on every resize and text change and pins its minimum height to that, so wrapped text is never clipped and never keeps stale space.
- `set_status_kind(label, kind)` / `set_ui_role(widget, role)` - switch QSS roles and re-polish (a bare `setObjectName()` leaves the old colour).
- `run_off_ui_thread(fn, *args)` - run a blocking call (adb) on a worker thread while the window keeps repainting; used by every GUI-thread adb call.
- `stretch_slider(slider, minimum)` - give a slider a minimum width and an Expanding policy so it fills its column instead of being pinned to a fixed track width.
- `NoScrollComboBox`, `NoScrollSlider`, `NoScrollSpinBox`, `NoScrollDoubleSpinBox` - scroll-wheel suppressed variants.
- `LogSliderRow` - slider + spinbox with logarithmic scaling (ISO, shutter speed).
- `PanSliderRow` - bipolar slider (−1 … +1).
- `create_separator()` - thin `QFrame` horizontal rule.
- `create_vector_icon(name, color)` - renders one of the inline SVG icons (24-unit grid, 2.2 stroke, soft fill) to a `QIcon`, tinted per use. Set: `connection`, `usb`, `camera`, `stream`, `gear`, `status`, `qr`, `play`, `stop`, `expand`, `reset`, `transforms`, `check`, `devices`. Unknown names give a blank icon.
- `ns_to_display(ns)`, `quality_label(q)` - display format helpers.

Note: white balance sliders are built directly in `plugins/camera_control.py`, not as a shared widget here.

### `widgets/qr.py`
**QRCodeWidget(data, module_px)** - QR code painted with QPainter, white quiet zone included. Used by Add phone and the checklist's APK link.

### `widgets/lens_panel.py`
**LensPanel** - two-column grid of lens buttons populated from the `cameras` list in the phone's `/v1/state` response. Labels are shortened (`shorten_lens_label`) and elide to the button's width. Emits `lens_selected(dict)` when the user switches lenses.

---

## `telescope/platform/`

### `platform/__init__.py`
Cross-platform constants and helpers: `IS_LINUX`, `IS_WINDOWS`, `adb_available()`, `adb_device_states()` (serial + state, including `unauthorized`), `adb_devices()`, `adb_forward(port)`, `adb_forward_auto(remote_port, serial)` (adb picks the local port), `adb_unforward(port)`, `adb_reverse(port)`, `adb_unreverse(port)`, `adb_broadcast_pair(payload)`, `adb_exe()`, `bundled_apk_path()`, `_run(cmd)`. `_run()` returns an error tuple instead of raising when adb isn't installed.

### `platform/linux.py`
v4l2loopback helpers: `v4l2_module_installed()`, `v4l2_load()`, `v4l2_unload()`, `v4l2_module_loaded()`, `v4l2_devices_ready()`, and the load-at-boot trio `v4l2_persist_status/enable/disable()`. Device constants: `V4L2_PHONE_DEV = /dev/video11`, `V4L2_OBS_DEV = /dev/video10`.

### `platform/windows.py`
UnityCapture helpers: `uc_is_registered()`, `unitycapture_dir()`, `download_unitycapture()`, `register_unitycapture()`.

---

## `telescope/plugins/`

### `plugins/connection.py`
**ConnectionPlugin** - paired phones, how they're reached, and pairing.
- Card rows: **Phone** (status), **Using** (live route), **Connect via** (Automatic / USB only / Wi-Fi only), a note with the fix when something's wrong or why a plugged-in cable isn't used, and **Switch to USB** when the phone is plugged in mid-stream over Wi-Fi. The card header's action is **Add phone**. While streaming, the status reads Connecting… until `bus.stream_connected`.
- `create_header_widget()` returns the phone picker and the Your phones button.
- `status_line()`, `problem_text()`, `route_text()` - module-level text for each state, shared by the card and Start failures.
- `_check_status()` every 3 s while idle resolves the selected phone in a background thread (`_spawn_resolve`); results carry a check id and are dropped when stale. A Wi-Fi address that answers is remembered as the phone's `active_ip`.
- `get_stream_info()` → `(url, token, ok)` - makes sure the virtual camera is loaded (Linux), resolves the route, and for USB holds a `UsbTunnels` forward to 8080 until `on_stream_stop()`.
- `session_target()` → `SessionTarget(token, route)` - read on the GUI thread and handed to worker threads.
- `session_channel(target)` - context manager yielding a `PhoneSessionClient` on the target's route (USB through a refcounted forward), or `None`.
- `ensure_phone_streaming(on_progress=None, target=None)` → `(ok, reason)` - starts the phone's camera if needed and polls `/v1/ping` until it streams (12 s budget). **Blocking - background thread only.**
- `stop_phone_streaming(target=None)` - best-effort `POST /v1/session {"action":"stop"}`. **Blocking.**
- `AddPhoneDialog` - QR for Wi-Fi when there's a network; polls `adb devices` every 2 s and re-sends the USB offer (`adb reverse` + broadcast) every 4 s to each usable device; says so when a device is waiting on the USB debugging prompt.
- `PhonesDialog` - list with Add phone / Rename / Remove, plus this computer's name as phones see it. Remove calls `forget_phone()`: unpairs on the phone when reachable (`/v1/unpair`) and deletes the phone's stored settings.
- Re-pairing the phone that's streaming reconnects the stream with the new token.
- Emits `bus.phones_changed` on every list change and opens Add phone on `bus.add_phone_requested`.
- Config keys: `computer_id`, `computer_name`, `route`, `phones`, `selected_phone`.

### `plugins/camera_control.py`
**CameraControlPlugin** - lens selection, exposure, white balance, focus, OIS, and image tuning. `panel_region = "right"`.
- UI: `LensPanel`, camera capability info label, then sections: Exposure (auto/manual, ISO, shutter, compensation), White balance (auto/manual, Kelvin, tint), Focus (auto/manual, distance), Image (OIS, noise reduction, sharpening, black-level lock, torch).
- `derive_camera_control_view(state)` - pure function mapping a raw phone-state dict to a `CameraControlView` dataclass, independently testable without a `QApplication`.
- `on_stream_start`: stores ctrl, sets "Loading lenses..." placeholder, re-pushes desktop-restored state to phone (phone keeps boot defaults until user touches a control).
- `on_phone_state(state)`: loads cameras into `LensPanel`, syncs exposure/WB/focus/OIS/AE-comp/NR/edge/black-level-lock/torch from phone state. Empty `state` dict (fetch failure) shows "Unavailable" on lens panel.
- `on_stream_stop`: clears lens panel and info label.
- `_update_camera_caps()`: disables manual exposure, manual WB, manual focus, or torch controls when the selected lens doesn't report support for them.
- Config keys: `exp_manual`, `iso`, `shutter_ns`, `ois`, `focus_manual`, `focus_diopters`, `wb_manual`, `wb_kelvin`, `wb_tint`, `ae_comp`, `nr_mode`, `edge_mode`, `bll`.

### `plugins/stream_output.py`
**StreamOutputPlugin** - capture resolution, frame rate, and encoding settings.
- UI: aspect-ratio and resolution combos from the current lens's `supportedSizes` (dynamic, not fixed) - sends a live `resolution` control instead of a post-decode resize. FPS spinbox (5-60) drives both phone capture and virtual-camera playback. JPEG quality slider (1-100%; the High/Balanced/Low wording is in its tooltip).
- Keeps the last resolution the device used, so saves made while idle (combos cleared) or during a device switch don't drop it.
- `_apply_camera()` rebuilds resolution combo on lens change, carries current selection forward (reuses existing capture size) instead of resetting to largest; reflects live stream size on reconnect if it differs.
- `get_stream_params()` → `(width, height, fps)` - width/height are always `None` (resolution is phone-controlled, not desktop-resized); called by `app.py._start()` to construct `StreamWorker`.
- `on_stream_start`: stores ctrl, schedules `_push_initial_settings` (1500ms delay) to sync quality/fps after connect.
- `_on_resolution()` sends `resolution` control and emits `bus.resolution_change_requested` (used by `app.py` for footer readout). `_on_fps()` sends `fps_target` and calls `host.update_stream_output()` for virtual-camera hot-swap (no stream restart).
- Config keys: `resolution`, `fps` (falls back to reading legacy `phone_fps` if `fps` is absent), `jpeg_quality`.

### `plugins/preview.py`
**PreviewPlugin** - the centre video stage and its pop-out. `panel_region = "center"`.
- UI: letterboxed frame and a toolbar with Hide/Show toggle and Pop out.
- Active by default - it's the centre of the window, not an opt-in card. The toggle remains as an escape hatch for anyone who'd rather not spend the decode.
- `process_frame(frame)` - runs on stream-reader thread; records pre-downscale size, downscales to `_CARD_MAX_W` for in-window (full res for pop-out), emits cross-thread Qt signal, returns frame unmodified (preview-only).
- Pop-out window auto-hides the in-card preview when opened. While the main window is hidden (tray) the card stops decoding without changing its Hide/Show setting.
- Hides the whole stage while `bus.setup_needed` is true, giving the column to the first-run checklist.
- No config keys - preview visibility isn't persisted across restarts.

### `plugins/onboarding.py`
**OnboardingPlugin** - the first-run checklist. `panel_region = "center"`.
- Three steps: virtual camera (`v4l2_module_installed()` on Linux, with the package names if it's missing; one-click UnityCapture install on Windows), phone app (QR to `APK_URL`, or install over USB when a bundled APK and adb are present), Add phone (emits `bus.add_phone_requested`).
- Shown while not streaming and until a phone is paired and the virtual camera can work; announces that with `bus.setup_needed`.
- No config keys: its state is derived, never stored.

### `plugins/transforms.py`
**TransformsPlugin** - software frame transforms applied in the stream pipeline. `panel_region = "left"` (desktop-side processing, next to the output settings).
- UI: flip (H/V segmented), rotation (None / 90 CW / 180 / 90 CCW), zoom slider (1×-5×), pan left/right and up/down (enabled only when zoomed), and a Reset action in the card header that drives the widgets so the handlers do the rest.
- `process_frame(frame)` - applies zoom crop then flip/rotate; runs on the worker thread. Reads plain Python attrs (`flip_h`, `flip_v`, `rotation`, `zoom`, `pan_x`, `pan_y`) written by the Qt thread; GIL makes these reads atomic.
- Config keys: `flip_h`, `flip_v`, `rotation`, `zoom`, `pan_x`, `pan_y`.

### `plugins/monitoring.py`
**MonitoringPlugin** - battery/temperature display and alerts. `panel_region = "right"` (the phone's own health, beside its camera).
- UI: live battery % + temp display, alert threshold spinboxes (battery %, temp °C).
- Subscribes to `bus.phone_state_updated`; also polls independently every 15 s via a daemon thread + `_Signals` inner class for thread-safe emit.
- Calls `host.send_notification()` for battery-low and overheating alerts (once per threshold crossing).
- Config keys: `battery_alert`, `temp_alert`.

### `plugins/setup.py`
**SetupPlugin** - the **Advanced** dialog, reached from the header's settings menu (`create_menu_actions()`); no panel, since nothing in it is adjusted mid-stream.
- `AdvancedDialog`: v4l2loopback status, load/unload and load at boot (Linux), UnityCapture status and reinstall plus adb status (Windows), installing an APK over USB, and the virtual camera canvas (presets or custom, applied through `host.restart_vcam_canvas()`).
- `get_canvas_dims()` → `(w, h)` or `(None, None)` for auto; read by `app.py` at stream start.
- Config keys: `canvas_preset`, `custom_canvas_w`, `custom_canvas_h`.
