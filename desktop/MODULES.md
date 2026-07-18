# Telescope Desktop — Module Reference

Quick index of what lives where. Detailed behaviour is in the source; this is for navigation.

---

## Entry point

### `main.py`
Dependency check, Qt app setup, theme application, single-instance guard, plugin registration, config restore, event loop.
Registration order: `SetupPlugin → ConnectionPlugin → CameraControlPlugin → StreamOutputPlugin → TransformsPlugin → PreviewPlugin → MonitoringPlugin`.
Calls `win.apply_saved_config()` **after** all plugins are registered so every plugin's `set_config()` is available.

---

## `telescope/` package

### `app.py`
**TelescopeWindow** — thin coordinator shell.
- Owns the scroll area, footer (status, FPS, Start button), tray icon.
- Owns `EventBus` and `StreamWorker` lifecycle.
- `register_plugin(p)` — calls `p.setup()`, inserts `p.create_panel()` before the trailing stretch in the scroll area.
- `apply_saved_config()` — call after all plugins are registered; restores config round-trip for each plugin.
- `_start()` — calls `conn.get_stream_info()` for URL (validates ADB/v4l2, builds URL), queries `stream_output.get_stream_params()` for worker dimensions.
- `_stop()` — tears down worker and ctrl; `on_stream_stop()` on each plugin (ConnectionPlugin unforwards ADB there).
- Implements the public **`HostServices`** contract plugins call on `host` (see `plugin.py`): `schedule_save()`, `save_now()`, `switch_device()`, `reconnect_stream()`, `send_notification()`, `is_streaming()`, `stop_stream()`, `update_stream_output()`, `restart_vcam_canvas()`. Plugins go through these public methods only — they never touch private (`_`-prefixed) window internals like `_worker`/`_stop()`.
- `send_notification(title, body)` — uses `notify-send` on Linux, tray balloon on Windows.
- `save_now()` — writes global plugin configs (connection, setup) and per-device configs (camera_control, stream_output, transforms, monitoring) under `devices[selected]`. `schedule_save()` is the debounced variant plugins call after a settings change.
- `switch_device(prev, new)` — saves `prev` device's per-device configs, restores `new` device's; called by `ConnectionPlugin._on_device_changed()`.
- `is_streaming()` / `stop_stream()` / `update_stream_output(width, height, fps)` — public stream controls; `stop_stream()` is a guarded no-op when idle, and `update_stream_output()` forwards only the values a caller passes (`None` width/height means pass-through).
- `_plugin(name)` — central typed lookup of a registered plugin by name (replaces scattered inline `next(p for p ...)` scans).
- `_apply_config(cfg)` — routes global plugin slices to `p.set_config()`, per-device slices for the selected device, then calls `conn.select_device()` to set the active device in the combo.
- Utility exports: `acquire_single_instance()`, `listen_for_raise()`, `EXTRA_QSS`.

### `plugin.py`
**TelescopePlugin** base class + **HostServices** contract + **EventBus**.
- `HostServices` (typing.Protocol): the public surface a plugin may call on its `host` handle — `schedule_save`, `save_now`, `switch_device`, `reconnect_stream`, `send_notification`, `is_streaming`, `stop_stream`, `update_stream_output`, `restart_vcam_canvas`. Structural typing only (`TelescopeWindow` implements it without inheriting). Keeps plugins off private window internals.
- `UNCHANGED`: sentinel for `update_stream_output` so `None` can be passed as a real value (pass-through resolution) distinct from "leave as-is".
- `TelescopePlugin`: override `setup`, `create_panel`, `on_stream_start`, `on_stream_stop`, `on_phone_state`, `process_frame`, `get_config`, `set_config`.
- `EventBus(QObject)`: signals — `frame_ready`, `stream_start_requested`, `stream_stop_requested`, `stream_started`, `stream_stopped`, `phone_state_updated`, `device_changed`.

### `stream.py`
**StreamWorker(QThread)** — video capture and virtual camera output.
- Reads the authenticated MJPEG stream via `telescope/mjpeg_reader.py`'s `MjpegReader` (bearer token in the request header), writes to `pyvirtualcam`.
- `frame_pipeline: list[Callable]` — each callable receives an RGB numpy array and returns one; applied in order after resize.
- `update_output(width, height, fps)` — hot-swap output resolution/FPS without stopping the worker.
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

`DEVICE_LOCAL_PLUGINS` frozenset marks which plugin names are per-device. Migration handles: v0 (original flat format with single `ip` field), v1 (Phase 3 flat `plugin_configs` dict), and v2 passthrough.

### `phone_client.py`
**PhoneControlClient** — authenticated HTTP client for the phone app's `/v1/state` and `/v1/control` endpoints (bearer token on every request).
- `send(action, **kwargs)` — fire-and-forget control command.
- `get_state()` — fetch current camera state dict (lenses, ISO, shutter, WB, battery, etc.).

---

## `telescope/widgets/`

### `widgets/common.py`
Reusable Qt widgets and helpers used across multiple panels:
- `NoScrollComboBox`, `NoScrollSlider`, `NoScrollSpinBox`, `NoScrollDoubleSpinBox` — scroll-wheel suppressed variants.
- `LogSliderRow` — slider + spinbox with logarithmic scaling (ISO, shutter speed).
- `WbSliderRow` — white balance Kelvin slider with preset snapping.
- `PanSliderRow` — bipolar slider (−1 … +1) with centre-reset button.
- `create_separator()` — thin `QFrame` horizontal rule.
- `create_vector_icon(name, color)` — renders an SVG icon to `QIcon`.
- `ns_to_display(ns)`, `quality_label(q)`, `wb_name(k)` — display format helpers.

### `widgets/lens_panel.py`
**LensPanel** — horizontal list of lens buttons populated from the phone's `/cameras` response. Emits `lens_selected(dict)` when the user switches lenses.

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
**ConnectionPlugin** — mode selection, device list, port, ADB lifecycle. Registered first.
- UI: USB/Wi-Fi radio buttons, device combo (+/− buttons, IP display), port field.
- `get_stream_info()` → `(url, ok)` — validates port, checks v4l2loopback (Linux), ADB-forwards if USB mode; called by `app.py._start()`. Shows error dialogs on failure.
- `on_stream_stop()` — unforwards ADB if a forward was established this session.
- `_AddDeviceDialog` (module-private) — dialog for adding a named Wi-Fi device.
- Config keys: `mode`, `port`, `devices_list` (no `selected_device` — it lives at config top-level).
- `select_device(name)` — called by host after `set_config()` to set the combo selection without triggering device-change logic.
- `DEFAULT_PORT = 8080` defined here.

### `plugins/camera_control.py`
**CameraControlPlugin** — lens selection, exposure, white balance, OIS.
- UI: `LensPanel` (horizontal lens buttons), camera capability info label, Exposure auto/manual + ISO + shutter sliders, White Balance auto/manual + Kelvin slider, OIS checkbox.
- `on_stream_start`: stores ctrl, sets "Loading lenses..." placeholder.
- `on_phone_state(state)`: loads cameras into `LensPanel`, syncs exp/wb/ois from phone state. Empty `state` dict (fetch failure) shows "Unavailable" on lens panel.
- `on_stream_stop`: clears lens panel and info label.
- `_update_camera_caps()`: disables manual exp or manual WB buttons when the selected lens doesn't support them.
- Config keys: `exp_manual`, `iso`, `shutter_ns`, `ois`.

### `plugins/stream_output.py`
**StreamOutputPlugin** — output resolution, frame rate, and encoding settings.
- UI: resolution combo (pass-through / 1080p / 720p / 480p / 360p), playback FPS spinbox, JPEG quality slider, phone FPS spinbox.
- `get_stream_params()` → `(width, height, fps)` — called by `app.py._start()` to construct `StreamWorker`.
- `on_stream_start`: stores ctrl, schedules `_push_initial_settings` via `QTimer.singleShot(1500)` to sync quality/fps to the phone after connect.
- Resolution and FPS changes call `host._worker.update_output()` for hot-swap without stream restart.
- Config keys: `resolution`, `fps`, `jpeg_quality`, `phone_fps`.

### `plugins/preview.py`
**PreviewPlugin** — in-card and pop-out live video preview.
- UI: "Show"/"Hide" toggle for an in-card preview label, "Pop out" button opening a floating, aspect-ratio-locked `_PopoutWindow`.
- `process_frame(frame)` — runs on the stream reader thread; downscales to `_CARD_MAX_W` for the in-card view (full resolution for the pop-out), emits a cross-thread Qt signal rather than touching any `QWidget` directly, then returns the frame unmodified (preview-only, doesn't alter the pipeline).
- Pop-out window auto-hides the in-card preview when opened, and closes/restores state when the main window is hidden (tray minimize).
- No config keys - preview visibility isn't persisted across restarts.

### `plugins/transforms.py`
**TransformsPlugin** — software frame transforms applied in the stream pipeline.
- UI: flip (H/V), rotation (None / 90 CW / 180 / 90 CCW), zoom slider (1×–5×), pan X/Y (enabled only when zoomed).
- `process_frame(frame)` — applies zoom crop then flip/rotate; runs on the worker thread. Reads plain Python attrs (`flip_h`, `flip_v`, `rotation`, `zoom`, `pan_x`, `pan_y`) written by the Qt thread; GIL makes these reads atomic.
- Config keys: `flip_h`, `flip_v`, `rotation`, `zoom`, `pan_x`, `pan_y`.

### `plugins/monitoring.py`
**MonitoringPlugin** — battery/temperature display and alerts.
- UI: live battery % + temp display, alert threshold spinboxes (battery %, temp °C).
- Subscribes to `bus.phone_state_updated`; also polls independently every 15 s via a daemon thread + `_Signals` inner class for thread-safe emit.
- Calls `host.send_notification()` for battery-low and overheating alerts (once per threshold crossing).
- Config keys: `battery_alert`, `temp_alert`.

### `plugins/setup.py`
**SetupPlugin** — "Drivers & APK" panel card wrapping **SetupDialog**.
- UI: single button that opens the dialog; no stream lifecycle hooks.
- `SetupDialog` handles: v4l2loopback status/load (Linux), UnityCapture install (Windows), ADB status (Windows), APK install via ADB.
