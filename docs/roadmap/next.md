# Next release

The next release follows 5.1.2. Ticked here means merged and not released yet.

- [ ] [Auto colour grading: whole look and always on](#auto-colour-grading-whole-look-and-always-on)
- [ ] [Graphics card choice on laptops with two cards](#graphics-card-choice-on-laptops-with-two-cards)
- [ ] [A writing pass over docs](#a-writing-pass-over-docs)

## Auto colour grading: whole look and always on

Carried over from 5.1. Still open is a whole-look button that also turns on auto framing and a light background blur, and an always-on mode. The always-on mode would need heavy smoothing to stop the picture from pumping when the light changes.

The whole-look button probably stays small, since it chains pieces that already exist. The always-on mode is medium, mostly the smoothing. Both would land in `rust/crates/effects/src/auto_grade.rs`.

## Graphics card choice on laptops with two cards

[#489](https://github.com/LunarKittyy/Telescope/issues/489) asked for Telescope to stay on the built-in graphics card of a laptop that has two, and it's still open. 5.1.2 puts the cards in an order of its own (real graphics cards first, Vulkan before DirectX 12) and skips a card that crashed the last start. A laptop whose Intel Vulkan driver crashes now ends up on the other card. That fixes the crash but doesn't put the window on the Intel card.

Still open is whether DirectX 12 works on that Intel card, which the reporter can try by starting Telescope with `WGPU_BACKEND=dx12` and `WGPU_POWER_PREF=low`. If it does, a graphics card choice in Settings would let people pick the card themselves instead of relying on the order. The order is in `rust/crates/core/src/gpu_pick.rs` and the crash marker in `rust/crates/core/src/gpu_marker.rs`. The window picks its card in `rust/crates/shell/src/preview.rs` and the picture work in `rust/crates/media/src/gpu.rs`, and both would need to follow the choice. Small to try, medium with a Settings choice.

## A writing pass over docs

A pass over everything in `docs/` against the writing rules: plain and short, British spelling, no AI tells, and each file's `covers` line still right. Docs only, but many files. Medium.
