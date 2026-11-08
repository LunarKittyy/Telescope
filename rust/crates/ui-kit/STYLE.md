# UI kit design guide

Every card is built from the widgets in `ui/widgets`. This is what they share, so a later restyle changes a kind in one place. Tokens live in `ui/theme.slint`.

## The only knobs

Three base values drive every size. Widgets never write a literal radius, gap or font size.

| Knob | Token | Now | Everything else |
|---|---|---|---|
| Text size | `Theme.body` | 13px | `small`, `tiny`, `card-title`, `icon` are ratios of it |
| Corner roundness | `Theme.radius` | 6px | `r-ctl`, `r-card`, `r-handle` are ratios of it |
| Spacing | `Theme.unit` | 2px | `Theme.u(n)` is n units; `row`, `gap`, `pad`, `stroke`, `track`, handle and switch sizes are multiples |

A setting for roundness, text size or density would only change one of these three.

## Colour tokens

Two looks share the same tokens: the new one and Classic (the deep blue of 5.0). Each token has one value per look, set by `Theme.classic`. The old names (`bg`, `surface`, `border`, `text-dim` and so on) are aliases for cards not yet moved over.

| Token | For |
|---|---|
| `stage`, `chrome`, `ground` | Behind the preview, the title and status bars, the window |
| `card` | A card's surface, one tone above the window |
| `raise` | A control's resting surface: picker, button, segment, switch track, slider track |
| `hover`, `sel` | A control under the pointer, a pressed or open control, a tooltip |
| `trough` | Recessed: a disabled box, an off switch's inside |
| `line`, `line-strong`, `line-hover` | The few real lines (a divider that splits meaning, a pad's crosshair) |
| `ink` | Primary text, values |
| `ink2`, `faint` | Secondary text: nearly white, so hierarchy comes from size and weight |
| `dis` | Disabled |
| `accent`, `accent-soft` | Links and the keyboard focus ring |
| `fill` | The one filled violet button (Start streaming) |
| `ok`, `warn`, `err` | Status |
| `plugin-hue(group)` | The card's own colour, by plugin group (camera, look, vision, preview, on_screen, sound, streaming, extras) |

Category colours stay: a card's hue goes on its icon and on its controls' active state. `Theme.muted(hue)` is the hue quietened for rest, `Theme.tint(hue)` the muted inside of an active control.

`tests/colours.rs` measures the category hues in OKLCH, in both looks: each within 0.04 of the middle lightness, chroma under 0.14, at least 3:1 against the card, and at least 30 degrees of hue from `ok`, `warn` and `err` (greys have no hue to compare). A colour that fails is listed in the test's `KNOWN` with why, and the test fails again when a colour changes either way. Add a ninth hue and the test says whether it fits.

## Type

One family, IBM Plex Sans, bundled in `ui/fonts` (regular, medium, semibold, bold, with its OFL licence beside them) and imported by `theme.slint`, so the window never depends on a system font. A few styles, weight and size carrying the hierarchy (no capitals, no tracking tricks: some scripts have no case). Plex doesn't cover Hindi, Japanese, Korean or Chinese: those fall back to a system font. `Theme.line-factor` puts a wrapped line on Plex's whole-pixel pitch, and a text that needs a baseline by hand uses Plex's ascent of 1.025 em.

| Style | Size | Weight | Use |
|---|---|---|---|
| Title | `card-title` | 600 | Card titles |
| Value | `body` | 600 | A current value, a picked option, a button label |
| Label | `body` | 500 | Parameter names, option text |
| Section | `small` | 600 | A group's title inside a card |
| Unit | `tiny` | 500 | Units and captions beside a value |

Text that stays in a card is styled as something: a status, a badge, a key, an empty state. No loose explaining sentences.

## Text

The window has no explaining text. A sentence that explains goes in a tooltip (one friendly sentence) or is dropped. What stays is styled as something: a status (a coloured word or short phrase), a badge (`Tag`), a key, an empty state (dim placeholders of the real thing's shape, or a short name like "No devices yet"). Something that can't be used yet is a disabled control with a tooltip saying why ("Start streaming to take photos"), never a sentence in the card. A menu or picker button never ends in "...": the picker's chevron already says it opens.

## Stability

The window doesn't jump when a state changes (streaming starts or stops, a device connects, a plugin turns on). Either the move is one the eye can follow, or the empty state keeps the space of what will appear: a placeholder roughly the shape and height of the real content, like the Camera's lens row, which is three dim tiles until the phone names its lenses.

The swap from placeholder to content happens in one frame. Never a frame where the old thing is gone and the new one isn't there yet (a model cleared then refilled, an `if` that removes one element before another is made, a size that collapses while data loads). Build it so both come from the same state change: update a model in place, switch visibility in one property change, keep the reserved height until the new content is laid out. `app/tests/card_stability.rs` steps a card through a state change one loop turn at a time and checks its height only ever goes from the one height to the other.

## Lines

No hairline border round a box. Surfaces separate by tone steps and spacing. A line is only for something that splits meaning. The thick outline (`Theme.stroke`) is for state, below.

## States

- Rest: muted. Raised surface, ink2 text, muted colour.
- Hover: one tone up (`hover`).
- Active (picked segment, lit switch): a thick border in the card's hue with a muted inside (`tint`), not a flat fill.
- Grabbed (a slider held): full colour, handle included.
- Disabled: `dis` text on a `trough` surface, no hue.
- Keyboard focus: the accent ring (`FocusRing`), only after Tab or an arrow key.

## Control kinds

Each exported widget says its kind in code: `out property <string> control-kind`. A test checks every widget has one from this list and that cards outside the kit draw no controls of their own.

| Kind | Widget | May vary |
|---|---|---|
| `card` | Card | title, icon, hue, header action or switch |
| `row` | FormRow | label |
| `section` | SectionHeading, Section | title, folding |
| `button` | Button | text, icon, role (plain, primary, danger, quiet) |
| `icon-button` | IconButton | icon |
| `link` | LinkText | text, address |
| `switch` | Switch | hue, danger |
| `toggle` | SwitchTile | icon, name, hue |
| `slider` | Slider | range, fill, readout, marks |
| `segmented` | Segmented | options, icons, hue |
| `picker` | ChoiceField | options, icon, caption |
| `field` | TextField, NumberField | text, placeholder, icon |
| `jog` | Jog | range |
| `stepper` | Stepper | range |
| `strip` | SettingRow | title, controls |
| `badge` | Tag | text, tint |
| `dialog` | DialogFrame | title |
| `tip` | TipBody | text |
| `icon` | Icon | picture, tint |
| `outline` | FocusRing, TouchRing | radius |
| `scrollbar` | ScrollBar, ColumnScrollbar, FlickBar | extent |
| `image` | QrCode | picture |
| `helper` | no drawing of its own (search words, tooltips' usage, shortcut menu) | nothing |

A menu button is always a `picker`: the value and a chevron, never "..." in a label. Glyphs inside controls (the chevron, a clear cross, a tick, a plus, the stepper's arrows) are heavier than the card icons: a 2px stroke against the icons' 1.4px, and the picker's chevron is 16px.

Every control in a card sits between the same two edges, the card's own: a label column then the control out to the right edge, or a row that runs across (`SwitchTile`, a full-width `Button`). No inset boxes, no tiles side by side that end at different places. A box that only groups (a status, a warnings list) isn't drawn: the rows sit on the card.

## Sliders

One anatomy for every slider: a track, a handle, the value on the right, and a tick at the centre when the slider goes both ways (a range that crosses zero, or `two-way`). Only the fill varies, by name, from a fixed list in `ui/widgets/fills.slint`. A card names a fill, it never draws one. Every fill has the same rounded ends as the track.

- `plain`: the card's colour from the start of the track up to the handle.
- `gradient`: the range's own colours along the whole track (`grad-a`, `grad-b`, `grad-c`), the handle taking the colour under it. Temperature, tint and saturation.
- `vibrance`: pastels that grow more saturated to the right, the handle taking the colour under it.
- `shadows` (black to grey), `highlights` (grey to white), `pivot` (black to white, so the handle marks the brightness contrast turns around).
- `lift` (dark), `gamma` (mid), `gain` (bright), `offset` (a plain grey ramp): the four sliders under the colour wheels.
- `contrast`: flat grey that fades into 45 degree black and white stripes, ending on a white one.

The grey ramps keep the card's colour on the handle, since black or white would vanish into the track.

At rest everything is muted: the fill and the handle's border in the quietened colour, the handle's inside in the card's. While the slider is held its full colour comes up, handle included, and dims when it is let go. A simple colour change for now; the animations are a later pass.

## Specimen sheet

Every kind in every state, side by side, in the new look and in Classic. Rows are kinds and columns are rest, on, hover, focus, held, disabled, placeholder and a long text (what a German translation gives). A kind with no cell in a column has no such state. Read it as a picture: a kind whose hover or held cell looks like its rest cell is the usual fault, and so is a long text that is cut off or runs out of its row.

```
cargo run -p telescope-shell --bin shell-specimen -- OUT
```

in `rust/` (with `SLINT_BACKEND=testing`, so nothing opens a window) writes `OUT/new.png` and `OUT/classic.png`. The cells are drawn from `crates/shell/ui/specimen.slint` and the grid is laid out in `crates/shell/src/specimen.rs`: a new kind or a new state is a line in its `KINDS` table and a block in the .slint file. Regenerate it after changing a widget and look at what moved.

## Adding or changing a control

Put it in `ui/widgets`, give it a `control-kind` from the table (add the kind here first if it is truly new), build it from tokens only, add it to the specimen sheet, and use it from the card. A card that needs a look the kit lacks asks for a new kind or a new variation of one, it doesn't draw its own.
