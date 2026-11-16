# Next release

The next release follows 5.1.2. Ticked here means merged and not released yet.

- [ ] [Auto colour grading: whole look and always on](#auto-colour-grading-whole-look-and-always-on)
- [ ] [Graphics card choice on laptops with two cards](#graphics-card-choice-on-laptops-with-two-cards)
- [ ] [A writing pass over docs](#a-writing-pass-over-docs)
- [x] [A Reset on every card and one for all settings](#a-reset-on-every-card-and-one-for-all-settings)

## Auto colour grading: whole look and always on

Carried over from 5.1. Still open are an always-on mode and a whole-look button that also turns on auto framing and a light background blur. The always-on mode would need heavy smoothing to stop the picture from pumping when the light changes.

The whole-look button is probably small because it chains pieces that already exist. The always-on mode is medium, mostly the smoothing. Both would land in `rust/crates/effects/src/auto_grade.rs`.

## Graphics card choice on laptops with two cards

[#489](https://github.com/LunarKittyy/Telescope/issues/489) asked for Telescope to run on the built-in graphics card of a laptop that has two and it's still open. 5.1.2 puts the cards in a fixed order (real graphics cards first, Vulkan before DirectX 12) and skips a card that crashed the last start. A laptop whose Intel Vulkan driver crashes now ends up on the other card. That fixes the crash but doesn't put the window on the Intel card.

Still open is whether DirectX 12 works on that Intel card (the reporter can try it by starting Telescope with `WGPU_BACKEND=dx12` and `WGPU_POWER_PREF=low`). A graphics card choice in Settings would let people pick the card themselves instead of relying on the order if it does. The order is in `rust/crates/core/src/gpu_pick.rs` and the crash marker in `rust/crates/core/src/gpu_marker.rs`. The window picks its card in `rust/crates/shell/src/preview.rs` and the picture work in `rust/crates/media/src/gpu.rs`. Both would need to follow the choice. Small to try, medium with a Settings choice.

## A writing pass over docs

A pass over everything in `docs/` against the writing rules (plain and short, British spelling, no AI tells and each file's `covers` line still right). Docs only but many files. Medium.


## A Reset on every card and one for all settings

Only a few cards have a Reset in their header today (Transforms, Colour grading, Desk, Webcam and the wait screen). Every built-in card that holds settings should get one, lit only while something on it is off its default. So should the Settings window's sections where it helps. Things people would hate to lose are left alone (paired phones, downloads, a loaded LUT). Settings also gets one button that resets every setting to its default after asking, leaves the paired phones alone and restarts Telescope.

The card button is already in the kit (`has-reset` in `rust/crates/ui-kit/ui/widgets/card.slint`). Most of the work is each plugin restoring its defaults and refreshing its card. Medium, spread over many small files.
