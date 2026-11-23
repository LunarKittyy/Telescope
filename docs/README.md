<!-- covers: ; checked: 1cbb8644b -->
# Telescope docs

New here? The [Quick Start](../README.md#up-and-running-in-about-two-minutes) is all most people need.

## For users

- [Features](features/README.md) is a tour of what the apps can do, grouped by area, with a link to the details for each
- [Troubleshooting](troubleshooting.md) has common problems and their fixes
- [Plugins](features/plugins.md) explains switching parts of the app off and installing plugins other people made
- [MCP server](features/mcp.md) covers letting an AI app like Claude Desktop read and change Telescope's settings
- [Manual setup](setup.md) covers the virtual camera by hand on Linux, loading it at boot, OBS as a Flatpak
- [Things to try](unusual-setups.md) has ideas like a phone from far away, a DSLR as a photo console, a whiteboard scene, 3D and the night sky
- [Device compatibility](device-compatibility.md) lists the phones that have been tested
- [Licenses](licenses.md) explains what the licence means for you and the third-party parts used

**Need help?** Ask in [Q&A](https://github.com/LunarKittyy/Telescope/discussions/categories/q-a) or share an idea in [Ideas](https://github.com/LunarKittyy/Telescope/discussions/categories/ideas). For bugs open an [issue](https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml) with your **Copy diagnostics** report.

## For contributors

- [Architecture](development/architecture.md) shows how the phone and desktop apps fit together
- [Desktop architecture](../rust/ARCHITECTURE.md) describes the crates and the plugin rules that make the desktop app work when parts are switched off
- [Control API](development/protocol.md) describes the phone's endpoints, the pairing QR code and the Browser camera messages
- [Writing a plugin](development/writing-plugins.md) is a guide with examples in Lua, plus the [WASM design](development/plugin-wasm.md) for later
- [Translating](development/translating.md) covers adding a language to either app and keeping new texts translatable
- [Building](development/building.md) covers building the apps and trying a change with a dev app
- [Packaging and CI](development/packaging.md) explains how the desktop app is packaged, the update path from the Python app and what each workflow does
- [Release checklist](development/release-checklist.md) lists what to test by hand before tagging a release
- [Automation](../automation/README.md) covers translating with AI models and release testing in a VM and the emulator
- [Upstream bugs](development/upstream-bugs.md) lists dependency bugs that are worked around, where the workaround is and when to drop it
- [Roadmap](roadmap/README.md) has what's planned, ideas for some day and what each past release set out to do
- [CONTRIBUTING.md](../CONTRIBUTING.md) is the short version of how to help
