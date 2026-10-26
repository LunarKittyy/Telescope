"""The built-in plugins. ARCHITECTURE.md has the rules they follow, docs/writing-plugins.md how to write one."""


def builtin_plugins() -> list:
    """One of each built-in plugin, in the order they're registered (the order their cards and menu entries come in).
    Only Connection is needed for streaming; any other can be left out (tests/test_plugin_removal.py checks)."""
    # Imported here, not at the top, so importing one plugin module doesn't import them all
    from telescope.plugins.background import BackgroundPlugin
    from telescope.plugins.browser_camera import BrowserCameraPlugin
    from telescope.plugins.camera_control import CameraControlPlugin
    from telescope.plugins.connection import ConnectionPlugin
    from telescope.plugins.framing import FramingPlugin
    from telescope.plugins.microphone import MicrophonePlugin
    from telescope.plugins.monitoring import MonitoringPlugin
    from telescope.plugins.on_air import OnAirPlugin
    from telescope.plugins.onboarding import OnboardingPlugin
    from telescope.plugins.presets import PresetsPlugin
    from telescope.plugins.preview import PreviewPlugin
    from telescope.plugins.setup import SetupPlugin
    from telescope.plugins.shortcuts import ShortcutsPlugin
    from telescope.plugins.startup import StartupPlugin
    from telescope.plugins.stream_output import StreamOutputPlugin
    from telescope.plugins.transforms import TransformsPlugin
    from telescope.plugins.updates import UpdatesPlugin
    from telescope.plugins.wait_screen import WaitScreenPlugin

    return [
        SetupPlugin(),
        ConnectionPlugin(),
        CameraControlPlugin(),
        BrowserCameraPlugin(),  # after Camera: its card takes that place while it's picked
        BackgroundPlugin(),  # optional: off until switched on in Plugins
        StreamOutputPlugin(),
        TransformsPlugin(),
        FramingPlugin(),  # optional, after Transforms: it moves the zoom and pan Transforms shows
        MicrophonePlugin(),
        PresetsPlugin(),
        PreviewPlugin(),
        OnboardingPlugin(),  # after Preview: the stage starts out making room for the checklist
        MonitoringPlugin(),
        UpdatesPlugin(),
        WaitScreenPlugin(),
        OnAirPlugin(),  # optional: off until switched on in Plugins
        ShortcutsPlugin(),
        StartupPlugin(),
    ]
