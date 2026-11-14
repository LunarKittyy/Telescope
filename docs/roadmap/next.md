# Next release

The next release follows 5.1.1. Ticked here means merged and not released yet.

- [ ] [Auto colour grading: whole look and always on](#auto-colour-grading-whole-look-and-always-on)
- [ ] [Colour picker near the top of the window](#colour-picker-near-the-top-of-the-window)
- [ ] [No reason shown when the phone pauses for heat](#no-reason-shown-when-the-phone-pauses-for-heat)
- [ ] [A flaky tooltip test](#a-flaky-tooltip-test)
- [ ] [A writing pass over docs](#a-writing-pass-over-docs)
- [ ] [Deleted files in the checklist check](#deleted-files-in-the-checklist-check)

## Auto colour grading: whole look and always on

Carried over from 5.1. Still open is a whole-look button that also turns on auto framing and a light background blur, and an always-on mode. The always-on mode would need heavy smoothing to stop the picture from pumping when the light changes.

The whole-look button probably stays small, since it chains pieces that already exist. The always-on mode is medium, mostly the smoothing. Both would land in `rust/crates/effects/src/auto_grade.rs`.

## Colour picker near the top of the window

5.1.1 stopped the Values switch moving away from the cursor. What's left is the picker opened near the top of the window, where the switch still moves about 3 px and the top of its bubble gets clipped. It lands in `rust/crates/ui-kit/ui/widgets/colour.slint`. Small.

## No reason shown when the phone pauses for heat

When the phone pauses the camera for heat, the desktop keeps saying Streaming and shows a frozen frame without saying why. The heat banner in `rust/crates/shell/src/sources.rs` (around line 322) covers the phone streaming smaller, not paused. Small if the pause already reaches the desktop, medium if the phone has to send it.

## A flaky tooltip test

`passing_over_controls_while_warm_does_not_flash_a_chain_of_tips` in `rust/crates/shell/tests/tooltips.rs` is flaky. Its assert around line 521 checks that no tip pops up while the pointer passes over controls. Small.

## A writing pass over docs

A pass over everything in `docs/` against the writing rules: plain and short, British spelling, no AI tells, and each file's `covers` line still right. Docs only, but many files. Medium.

## Deleted files in the checklist check

`.github/checklist_due.py` asks for a section when a deleted file isn't named in one. Line 103 uses `git diff --name-only`, which lists deletions, so it should skip them. Small.
