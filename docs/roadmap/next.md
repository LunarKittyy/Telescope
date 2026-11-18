# Next release

The next release follows 5.1.4. Ticked here means merged and not released yet.

- [ ] [Auto colour grading: whole look and always on](#auto-colour-grading-whole-look-and-always-on)
- [ ] [Rearranging the docs](#rearranging-the-docs)

## Auto colour grading: whole look and always on

Carried over from 5.1. Still open are an always-on mode and a whole-look button that also turns on auto framing and a light background blur. The always-on mode would need heavy smoothing to stop the picture from pumping when the light changes.

The whole-look button is probably small because it chains pieces that already exist. The always-on mode is medium, mostly the smoothing. Both would land in `rust/crates/effects/src/auto_grade.rs`.

## Rearranging the docs

The prose pass over `docs/` and the app's texts is done and went out in 5.1.4. What's left is rearranging the files themselves, so each topic is where people would look for it and each file's `covers` line is still right. Docs only but many files. Medium.
