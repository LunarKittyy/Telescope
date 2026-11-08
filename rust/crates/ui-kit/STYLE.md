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
| `hover`, `sel` | A control under the pointer, an open control, a tooltip |
| `press` | A control held down (one step above `hover`, so a press never reads as a hover) |
| `bubble` | A bubble's surface, a touch above the card so a control at rest in it still shows |
| `trough` | Recessed: a disabled box, an off switch's inside |
| `line`, `line-strong`, `line-hover` | The few real lines (a divider that splits meaning, a pad's crosshair) |
| `ink` | Primary text, values |
| `ink2`, `faint` | Secondary text: nearly white, so hierarchy comes from size and weight |
| `dis` | Disabled |
| `accent`, `accent-soft` | Links and the keyboard focus ring |
| `fill` | The one filled violet button (Start streaming) |
| `ok`, `warn`, `err` | Status |
| `plugin-hue(group)` | The card's own colour, by plugin group (camera, look, vision, preview, on_screen, sound, streaming, extras) |

Category colours stay: a card's hue goes on its icon and on its controls' active state. `Theme.muted(hue)` is the hue quietened for rest, `Theme.tint(hue)` the muted inside of an active control and `Theme.held(hue)` the same while it is held down.

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
- Grabbed (a slider held): full colour, handle included. Any other control held down goes to `press` (a lit switch or picked segment to `Theme.held`), never the same as hover.
- Disabled: `dis` text on a `trough` surface, no hue. It reads as there but unavailable: an icon button keeps its `trough` square, a slider keeps its handle outlined in `dis`.
- Long text: a label, option, readout, link, button or box that doesn't fit ends in an ellipsis and never leaves its row. A tip grows down instead.
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
| `bubble` | Bubble | what is inside, width |
| `colour` | ColourField | colour, hex and channels shown |
| `tip` | TipBody | name, detail, keys, tint |
| `icon` | Icon | picture, tint |
| `outline` | FocusRing, TouchRing | radius |
| `scrollbar` | ScrollBar, ColumnScrollbar, FlickBar | extent |
| `image` | QrCode | picture |
| `helper` | no drawing of its own (search words, `Tip`, `TipLayer`, shortcut menu) | nothing |

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
- `tolerance`, `spill`: two colours from the card (`grad-a` to `grad-b`, left to right) with the handle taking the one under it. Chroma key's tolerance runs from the screen's colour to a grey, its spill from a skin tone with the screen's colour on it to a clean one.
- `edges`: `grad-a` on the left, `grad-b` on the right and a step between them that is hard at the track's start and spreads over the whole track at its end, so the handle's place is the softness. The handle keeps the card's colour.

The grey ramps keep the card's colour on the handle, since black or white would vanish into the track.

Keys and typing, the same on every slider (the focused slider takes them; tested in `crates/shell/tests/slider_keys.rs`):

- Arrow keys step by one `step` (`key-small`).
- Shift with an arrow, Page Up and Page Down step by a tenth of the range in whole steps (`key-big`: 10 on 0 to 100, 100 on 0 to 1000).
- Home and End go to the minimum and the maximum.
- Enter on the slider, or a double-click on its value, opens a box with the number, unless the readout isn't that number (a word like "Chest"; one that starts with a digit, a plus or a minus is): then no box is offered and the keys above still work. Enter sets it (held to the range and rounded to the step, a non-number is dropped), Escape and clicking away leave the value as it was. After a key opened it, the focus goes back to the slider.
- A slider with `typing` set (a value that isn't its position, like a shutter time) opens its box on a click and hands the text to `typed` for the owner to read. `type-in: false` turns the number box off.

Every one of these says what a drag says, `moved` then `released`, and says nothing when the value didn't change, so an owner never has to know how a value was set.

At rest everything is muted: the fill and the handle's border in the quietened colour, the handle's inside in the card's (`tolerance`, `edges` and `spill` lie under a heavier veil, `Fills.rest-veil`, since their colours run bright along the whole track). While the slider is held its full colour comes up, handle included, and dims when it is let go. A simple colour change for now; the animations are a later pass.

## Bubbles

One popover for everything that needs one (the colour picker now; the live crop, the chasing dots and the like later). It is the kit's `Bubble`: a frame in the card's colour with a pointer at the control it belongs to, a `bubble` surface, `Theme.stroke` for its line and `r-bubble` for its corners. It sits inside the control it points at, as big as that control, and takes no room in the card, so opening it moves nothing. It opens above the control, flips below when it doesn't fit above, keeps its frame inside the window (`BubbleSpot` has the arithmetic) and puts the pointer over the control's middle. A press anywhere outside closes it and the card behind never sees that press; Escape closes it too. The focus goes in when it opens and back to the control when it closes. What goes inside is up to the caller, built from kit controls.

Nothing moves when it opens or closes yet. `reveal` is the one number to ease later, 0 closed and 1 open.

## Tooltips

Most cards will turn labels into icons and put the word in a tooltip, so a tooltip is how people learn the window. It is part of the kit's look, not a system popup: Slint's own `Tooltip` follows the pointer, waits a fixed 500 ms and can't be placed, so the kit draws its own. Don't write `Tooltip { }` in a card.

What it says, in three parts and only the first is needed:

- The name (`tip`): body size, semibold. Most tooltips are only this.
- A detail (`tip-detail`): a line or two in `small` and `ink2`, wrapping at about 36 characters (the box is at most 260px wide with its padding, `Theme.u(120)` of text). It explains, the name doesn't repeat itself. A text that reads "Name. Longer explanation" is split into the two.
- Keys: small keycaps on the right of the name. A control with a `shortcut` shows the keys of its saved shortcut by itself (`Shortcuts.key-chips`); `tip-keys` gives them by hand.

Look: the `sel` surface (the token for a tooltip), a 1px `line-hover` outline, `r-ctl` corners so the roundness setting moves it, `u(5)` by `u(3)` padding and a soft shadow (the software renderer the specimen uses doesn't draw shadows; the window does). Keycaps are `card` chips. All tokens, so Classic and a later look follow. A dot in the card's colour before the name is there (`hue` on `Tip` and `TipBody`) and the kit's controls don't use it: only the controls that are given the card's colour (switch, slider, segmented) could carry it, and a card whose buttons and fields have none would show half its tooltips tinted. A card that wants it passes `hue`.

Where and when: it sits below the control it explains, centred on it, `u(2)` away, and flips above when it would run off the bottom and there is more room above. It is held `u(4)` inside the window's sides and never follows the pointer (`TipSpot` has the arithmetic). The first one comes up after a rest of 500 ms. Moving straight onto another control shows its tooltip at once, and so does any control for a second after a tooltip went down. A press or the wheel on a control takes its tooltip down until the pointer leaves it. The keyboard focus (after Tab or an arrow key) shows it at once, so a tooltip is there for people who don't use a mouse.

How a control has one:

- A kit control takes `tip` (and `tip-detail`, `tip-keys`). It holds a `Tip`: `usage := Tip { says: root.tip; hover: touch.has-hover; inert: !root.enabled; focused: ...; }`, calls `usage.press()` and `usage.wheel()` from its touch area, and sets `inert` when its own touch area is off, so a disabled control can still say why.
- A text or a box with no touch area of its own holds `Tip { says: ...; inert: true; }`, which watches the pointer over it. Put it first among its siblings so it never covers something clickable. `enabled` turns it off (an elided text only says itself when it is cut).
- A `Tip` goes in a control (not in a layout) and covers it, so that is what the tooltip points at. It isn't under an `if`: nothing tells a tooltip its control was removed, though the window takes a tooltip down within a second of its control going.
- Every window has a `TipLayer` as its last child, which draws the tooltip over everything, and the kit's pop-ups (`DialogFrame`, `Bubble`) hold one each since a pop-up covers the window's. Without a layer a tooltip simply doesn't show.

`tests/tooltips.rs` has the timing, the placement and the parts; the specimen's second sheet (`tips-new.png`, `tips-classic.png`) draws a name, a detail, keys, a long detail, a tint and a tooltip flipped above and held in from the edge.

## Colours

`ColourField` is the one way to pick a colour: a swatch as wide as its row that shows the colour itself, in a raised well so a colour near the card's own still reads as a control. A click opens a bubble with a saturation and brightness pad, a hue strip, a pipette that picks from the preview (the plugin starts that pick) and, behind a small switch, the exact values (hex, red, green, blue). The pad and strip hold full colour, not the muted look of a slider's fill, since the colour is what they are for. The plugin keeps hue, saturation and brightness as the pad left them so a grey or a black keeps its hue, and stores whatever it already stored.

## Specimen sheet

Every kind in every state, side by side, in the new look and in Classic. Rows are kinds and columns are rest, on, hover, focus, held, disabled, placeholder and a long text (what a German translation gives). A kind with no cell in a column has no such state. Read it as a picture: a kind whose hover or held cell looks like its rest cell is the usual fault, and so is a long text that is cut off or runs out of its row.

```
cargo run -p telescope-shell --bin shell-specimen -- OUT
```

in `rust/` (with `SLINT_BACKEND=testing`, so nothing opens a window) writes `OUT/new.png` and `OUT/classic.png`. The cells are drawn from `crates/shell/ui/specimen.slint` and the grid is laid out in `crates/shell/src/specimen.rs`: a new kind or a new state is a line in its `KINDS` table and a block in the .slint file. Regenerate it after changing a widget and look at what moved.

## Adding or changing a control

Put it in `ui/widgets`, give it a `control-kind` from the table (add the kind here first if it is truly new), build it from tokens only, add it to the specimen sheet, and use it from the card. A card that needs a look the kit lacks asks for a new kind or a new variation of one, it doesn't draw its own.
