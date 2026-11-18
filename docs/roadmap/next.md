# Next release

The next release follows 5.1.4. Ticked here means merged and not released yet.

- [ ] [Auto colour grading: whole look and always on](#auto-colour-grading-whole-look-and-always-on)
- [ ] [More in presets](#more-in-presets)
- [ ] [An auto director](#an-auto-director)
- [x] [Rearranging the docs](#rearranging-the-docs)
- [x] [Doing the picture work on the GPU](#doing-the-picture-work-on-the-gpu)
- [x] [A graphics card choice in Settings](#a-graphics-card-choice-in-settings)
- [x] [Fixes for people tracking and Composite](#fixes-for-people-tracking-and-composite)
- [x] [Appearance options](#appearance-options)
- [x] [Stargazer and Phototrap](#stargazer-and-phototrap)
- [x] [Plugins that fail safely and share values](#plugins-that-fail-safely-and-share-values)
- [x] [DSLRs on Windows](#dslrs-on-windows)
- [x] [Spout capture fixes](#spout-capture-fixes)
- [x] [A faster interface underneath](#a-faster-interface-underneath)

## Auto colour grading: whole look and always on

Carried over from 5.1. Still open are an always-on mode and a whole-look button that also turns on auto framing and a light background blur. The always-on mode would need heavy smoothing to stop the picture from pumping when the light changes.

The whole-look button is probably small because it chains pieces that already exist. The always-on mode is medium, mostly the smoothing. Both would land in `rust/crates/effects/src/auto_grade.rs`.

## More in presets

Presets now save a Composite tile's zoom. Saving a USB webcam's settings, the microphone's gain, auto level and noise reduction and a Spout sender's frame rate is written and waiting to merge. Still being written are presets for Photos, Phototrap and Stargazer (with a test that every plugin's settings are covered) and Composite layouts in presets.

## An auto director

One picture picks its layout from who's in frame, like the face big while you talk and the desk big while you show something. A first version is written and off by default. It isn't merged yet.

## Rearranging the docs

The prose pass over `docs/` and the app's texts went out in 5.1.4. The files are now rearranged so each topic is where people would look for it. The feature docs were brought up to date after.

## Doing the picture work on the GPU

Background blur now runs on the graphics card (4K with everything on went from 5.5 to 2.4 ms for the blur). Spout capture is converted on the card too, the 1998 webcam costs much less and Green screen goes dark instead of showing the room when the card fails. What's left is in [future.md](future.md#doing-the-picture-work-on-the-gpu).

## A graphics card choice in Settings

Settings has a graphics card choice for someone whose system picks the wrong card. The guard that skips a card after a crash is tougher and VA-API follows the card you pick. The rest of load balancing (the integrated card, the front of the chain on the phone) is left out, see [dropped.md](dropped.md).

## Fixes for people tracking and Composite

- Composite tiles follow Max zoom and remember their zoom
- The stick figure copes with close-ups and crossed legs
- Auto framing can follow one person. Picking a person is refused on a preview that shows another stream
- Background cuts around chairs and light clothing more cleanly

## Appearance options

Look in Settings has options for corners, text size, density, higher contrast, motion and tooltip delay. Card colours is the one left, see [future.md](future.md#card-colours).

## Stargazer and Phototrap

Both are optional Lab plugins. Stargazer has lucky imaging, live stacking and star trails and shows how sharp the picture is. Phototrap takes a burst of photos when something moves in a zone you drew. The alignment and stacking they use is in `core` for other plugins to use too. Skipping photos while the picture is hidden has a few gaps left that are being closed.

## Plugins that fail safely and share values

A plugin that panics is switched off and the app carries on. A plugin can name the built-in plugins it needs in `plugin.json` and share values with other plugins through a host registry.

## DSLRs on Windows

Live view for Nikon and Canon works through Windows' own camera driver. Focus, aperture, exposure compensation and white balance work too. Only tried with a mock camera so far. Taking photos on Windows is written and waiting to merge. Both still need trying with a real camera.

## Spout capture fixes

A Spout sender that gets stuck on Linux no longer holds up Stop and its pictures are dropped when they finish too late. A sender that is stuck shows that on the card instead of an old picture, which is waiting to merge.

## A faster interface underneath

Fewer allocations while the window updates, about a third as many for each update from the phone. The window isn't visibly lighter. Nothing else is planned here.
