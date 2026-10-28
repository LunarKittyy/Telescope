# Settings files

Real and made-up `telescope_config.json` files, each next to an `expected.json` that says what the app does with it: whether it sets the file aside as unreadable, which plugins come up and what it saves straight after starting. `scripts/config_fixtures.py` writes the expected files and `tests/test_config_fixtures.py` checks the app still matches them. Anything that replaces the desktop app has to end up in the same place, so updating never costs anyone their pairings or settings.

| Folder | What it is |
|---|---|
| `linux-two-phones` | A real everyday setup: two phones, most optional plugins on, a LUT, Several cameras, nightly updates |
| `linux-dev-profile` | A real dev profile: a browser camera in Several cameras and a remembered slot for a phone that's gone |
| `windows-usb-phone` | Made up: one phone over USB, saved by Notepad with a byte order mark, Windows paths, shortcuts (one clashing, one with no keys, one for a plugin that's gone), presets with a repeated name, a webcam, browsers seen recently and long ago, an added plugin that isn't installed and a built-in one turned off |
| `damaged-sections` | Sections the app can't use (devices, collapsed cards, the selected device, broken phone entries, values out of range): each resets on its own and the rest stays |
| `version-2` | From 2.3 or earlier: global settings carry over, pairings and per-phone settings don't |
| `version-1` | Too old to read: set aside and started fresh |
| `future-version` | Saved by a newer Telescope: kept as it is, unknown parts included |
| `cut-off` | Half a file, as a crash or a full disk mid-save leaves it: set aside and started fresh |
| `fresh-install` | No settings file at all |

The real ones had their pairing tokens, certificate pins, ids, addresses, names and paths swapped for made-up ones.
