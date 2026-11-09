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

A column's scrollbar has its room always (`doc.rs`'s gutter: a gap and the 11 px bar, and a settings page's `ScrollPage` the same), so the bar appears in it and the cards neither move nor narrow. The last column's bar sits in the window's own right margin, flush with the window's edge.

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
| `row` | FormRow, RailRow, SwitchRow | label, or an icon and its tooltip word (FormRow only in Settings' pages, where the rows are named) |
| `section` | SectionHeading, Section | title, folding |
| `button` | Button | text, icon, role (plain, primary, danger, quiet) |
| `icon-button` | IconButton | icon (a raised square at rest; `bare` only for an icon inside a field) |
| `link` | LinkText | text, address |
| `switch` | Switch | hue, danger |
| `toggle` | SwitchTile, ToggleTile, PinStar | icon, name, hue; the star that pins a stat's number into the bottom bar |
| `slider` | Slider | range, fill, readout, marks, the live value of a threshold bar |
| `segmented` | Segmented, PictureSegmented, LensRow, AnchorFrame | options, icons or drawings, places in a frame, lenses that come and go, hue |
| `picker` | ChoiceField, PictureField | options, icon, caption; a file as a thumbnail and its name |
| `field` | TextField, NumberField, TextArea | text, placeholder, icon, skeleton lines |
| `pad` | Pad | ranges, the drawing on it, readout, a frame handle for a map |
| `wheel` | ColourWheel | the puck's place, the colour it adds, hue |
| `dial` | Dial | stops, which of them are named, whether it goes between them (`free`) |
| `jog` | Jog | range |
| `stepper` | Stepper | range |
| `strip` | SettingRow | title, controls: a row of a Settings card, on the card itself (no box of its own) |
| `badge` | Tag, ProblemMark | text, tint, detail (tooltip); the round red "!" and its reason |
| `status` | Status | one coloured word or short phrase, kind (quiet, ok, warn, err), detail (tooltip) |
| `list-row` | ListRow | name, a dim line, icon, the row's buttons: a raised strip in a list (dialogs, devices) |
| `dialog` | DialogFrame | title |
| `menu` | MenuSurface, MenuItem | the rows: a context menu, the Presets menu |
| `bubble` | Bubble, BubbleFrame | what is inside, width |
| `colour` | ColourField | colour, hex and channels shown |
| `tip` | TipBody | name, detail, keys, tint |
| `icon` | Icon | picture, tint |
| `outline` | FocusRing, TouchRing, Worm | radius, the picked cell's place (the inside is a `WormFill` under the text) |
| `scrollbar` | ScrollBar, ColumnScrollbar, FlickBar | extent |
| `image` | QrCode | picture |
| `readout` | PinReadout, PinChip | a pin of the bottom bar (its icon, value and place) and the "+N" that holds the pins that did not fit |
| `helper` | no drawing of its own (search words, `Tip`, `TipLayer`, `Peek`, `PeekLayer`, shortcut menu, `Unfolding`, `UnfoldGroup`, `RowHover`, `DialogButtons`, `LensArt`, `FineAxis`, the motions) | nothing |

A menu button is always a `picker`: the value and a chevron, never "..." in a label. Glyphs inside controls (the chevron, a clear cross, a tick, a plus, the stepper's arrows) are heavier than the card icons: a 2px stroke against the icons' 1.4px, and the picker's chevron is 16px.

Every control in a card sits between the same two edges, the card's own: a label column (an icon rail, below) then the control out to the right edge, or a row that runs across (`SwitchTile`, `ToggleTile`, a full-width `Button`). No inset boxes, no tiles side by side that end at different places. A box that only groups (a status, a warnings list) isn't drawn: the rows sit on the card.

## Sliders

One anatomy for every slider: a track, a handle, the value on the right, and a tick at the centre when the slider goes both ways (a range that crosses zero, or `two-way`). Only the fill varies, by name, from a fixed list in `ui/widgets/fills.slint`. A card names a fill, it never draws one. Every fill has the same rounded ends as the track.

- `plain`: the card's colour from the start of the track up to the handle.
- `gradient`: the range's own colours along the whole track (`grad-a`, `grad-b`, `grad-c`), the handle taking the colour under it. Temperature, tint and saturation.
- `temperature` (blue, grey, amber) and `tint` (green, grey, magenta): white balance, the same colours on every card, the handle taking the colour under it. Two ordinary sliders, never a pad: colour correction needs the whole track's travel for each and must not snap.
- `vibrance`: pastels that grow more saturated to the right, the handle taking the colour under it.
- `shadows` (black to grey), `highlights` (grey to white), `pivot` (black to white, so the handle marks the brightness contrast turns around).
- `lift` (dark), `gamma` (mid), `gain` (bright), `offset` (a plain grey ramp): the four sliders under the colour wheels.
- `contrast`: flat grey that fades into 45 degree black and white stripes, ending on a white one.
- `tolerance`, `spill`: two colours from the card (`grad-a` to `grad-b`, left to right) with the handle taking the one under it. Chroma key's tolerance runs from the screen's colour to a grey, its spill from a skin tone with the screen's colour on it to a clean one.
- `edges`: `grad-a` on the left, `grad-b` on the right and a step between them that is hard at the track's start and spreads over the whole track at its end, so the handle's place is the softness. The handle keeps the card's colour.
- `threshold`: a bar as tall as the row whose fill is a live value (`level` on the handle's own scale, `has-level` off while there is none), so the handle is the level to warn at. It is the one fill that changes the anatomy: no readout column, the handle's number (`readout`) sits beside the handle on the side with room and the live one (`level-text`, with an optional `level-icon`) at the far end away from the handle: right while the handle is in the left half, left once it passes the middle. The fill rounds its start like the bar and ends square where the value is; it is muted at rest and full colour while held, and with no live value the bar stays, empty, with a dash.

The grey ramps keep the card's colour on the handle, since black or white would vanish into the track.

Keys and typing, the same on every slider (the focused slider takes them; tested in `crates/shell/tests/slider_keys.rs`):

- Arrow keys step by one `step` (`key-small`).
- Shift with an arrow, Page Up and Page Down step by a tenth of the range in whole steps (`key-big`: 10 on 0 to 100, 100 on 0 to 1000).
- Home and End go to the minimum and the maximum.
- Enter on the slider, or a click on its value, opens a box with the number, unless the readout isn't that number (a word like "Chest"; one that starts with a digit, a plus or a minus is): then no box is offered and the keys above still work. Enter sets it (held to the range and rounded to the step, a non-number is dropped), Escape and clicking away leave the value as it was. After a key opened it, the focus goes back to the slider.
- A slider with `typing` set (a value that isn't its position, like a shutter time) opens its box on a click and hands the text to `typed` for the owner to read. `type-in: false` turns the number box off. A double-click on the track puts a slider that has a default (`has-default`) back to it.

Where the handle goes when it isn't the pointer taking it there: a key, a click on the track, a reset, a typed number and anything outside (Auto, a preset, the phone) glide it (`Settle`, a key's repeats retargeting it with its speed kept); only a hand on the handle puts it there at once. A slider whose step is a visible jump on the track (16 px or more: a 1 to 5 or 1 to 10 slider) glides between steps even while it is dragged, on a quicker settle (half the time), so the handle and the fill slide from step to step and a pointer that crosses several at once retargets it mid-flight. A press that lands on the handle sticks to the pointer with no lag from the first pixel; a press on the track glides to the pointer and sticks to it from the moment it arrives. On a slider with a default, a click on the track waits for the double-click interval (`Motion.double-click`, 350 ms) before the value and the handle move, so a double-click goes straight to the default and never to the click first; a drag that starts on the track, or a press held past the interval, takes hold without waiting.

Every one of these says what a drag says, `moved` then `released`, and says nothing when the value didn't change, so an owner never has to know how a value was set.

At rest everything is muted: the fill and the handle's border in the quietened colour, the handle's inside in the card's (`tolerance`, `edges` and `spill` lie under a heavier veil, `Fills.rest-veil`, since their colours run bright along the whole track). While the slider is held its full colour comes up, handle included, and dims when it is let go. The colour eases up and down (see Motion).

## Shift is finer

Shift while dragging goes a tenth as far on everything that is dragged: sliders, the pan map and every other pad, the dial, the jog, the colour pad and hue strip, the scroll bars, the colour wheel, the curve editor's points, the picture's pan and pick. One factor, `Fine.factor` (`ui/widgets/fine.slint`, the colour wheel's tenth; `WHEEL_FINE` in `effects/src/editors.rs` is the same number), and one way of keeping the value from jumping: a control keeps a `FineAxis` for each axis it is dragged along, calls `begin` with where the pointer went down and passes every move through `at(pointer, shift, lo, hi)`. Moves made with Shift held count a tenth, what was held back stays held back when Shift is let go (nothing jumps when it is pressed or released mid-drag), and what a value would be past an end is not forgotten, so coming back takes the same way back. Callers get it for free: nothing in a card handles Shift for a drag. A new draggable control uses `FineAxis` and `tests/design.rs` fails until it does (or it is listed there with why not: the composite editor keeps Shift for stretching, a pin follows the pointer, a menu only hovers). `crates/shell/tests/kit_fine.rs` has a test per kind that a Shift drag goes a tenth as far as a plain one.

## Tiles, pictures, pads and dials

Four controls that the card redesigns need, built on the same states as the rest (muted at rest, a thick border and muted inside when active, `press` or `Theme.held` while held, `dis` on `trough` when disabled, the accent ring from the keyboard). Each keeps its size in every state, empty or disabled included.

- `ToggleTile` (kind `toggle`): a `SwitchTile` without its switch. An icon and a name on a raised tile `Theme.tile` high, and the thick border in the card's colour is the state. With no text it is an icon tile (Previous, Play, Next), named by `name`. Space and Enter flip it.
- `PictureSegmented` (kind `segmented`): a `Segmented` whose options are drawings. Same tiles, same picking, same keys (Tab stops, arrows move and pick, Space picks). The drawing is the caller's, put inside the control: option `i` goes in the square from `x: i * pitch`, `cell-width` by `art-height`, and reads `shown` and its own `disabled` for how to look. `show-names` writes each option's name under its drawing, the drawing (`art-size` tall) and the name centred in the tile as one group; otherwise the name is the tooltip. If any name in the row is too long for its tile, no name in that row is drawn (never some and not others, never cut short or shrunk) and the tooltips carry them all. A drawing takes no pointer and draws no `border-width` (use a `Path` outline).
- `Pad` (kind `pad`): one box that sets two numbers (the Transforms card's pan map is the one use now: Desk mode's sheet size went back to a Width and a Height slider, and white balance is two sliders too), `value-x` across and `value-y` up and down (`y-up` says which way is more), each with its own range and step. Its height is 2 for every 3 of its width unless the caller says otherwise (about 230 px in the card's 342 px column), and in a layout it can't be squeezed flatter than 1 for every 2 of width, so both axes get similar travel. A crosshair marks where both are neutral (`centre-x`, `centre-y`), the handle is a circle that takes the card's colour held, with a halo. What is drawn on it (the sheet) and any button on it (a Reset) are its children; a drawing can follow the handle through `handle-x` and `handle-y`, and `handle-fill` puts the colour under the handle in the handle. `readout` is a chip at a corner. The pad rounds its own corners over any drawing. Arrow keys move one step, Shift moves a tenth of the range, Home and a double-click go to the default (`has-default`, else the centre). It says `moved` then `released` with both numbers, like a slider, and nothing when neither changed. No typed value yet.

  With `box-w` and `box-h` the handle is a frame instead of a puck, for a crop on a map: the frame's middle runs from half its size in, so it always lies on the pad, what is outside it is dimmed (`Theme.stage` at 0.6), and its outline is the card's colour (quiet at rest, full colour while held or hovered, tinted inside while held, `dis` when disabled). A press inside the frame keeps hold of where it was taken, so it follows the pointer's own distance and never jumps; a press outside brings the frame's middle there. The pad keeps its drawing when disabled (the frame goes grey, no veil over it), `has-centre: false` leaves out the crosshair, `spoken` is what a screen reader says in place of the readout, and `shortcut-adds` gives the add items of a pad that stands for two actions. `held` says a press has the handle.
- `ColourWheel` (kind `wheel`): a puck on a disc inside a ring of hues, for a colour that is added to the picture (lift, gamma, gain, offset). The ring is full colour (what the wheel is for, like the colour picker's pad), the disc a `raise` well that lightens under the pointer, with a quiet crosshair and two rings, never a scale. The puck is filled with the colour it adds and edged in the card's colour (quiet at rest, full colour held or hovered), takes a halo while held, and a line runs back to the middle. It never snaps and a drag never jumps to the pointer: it says `pressed`, `dragged(dx, dy, shift)` with the pointer's distance from where it went down in radii of the disc, and `released`, and the owner moves the puck by what the pointer travelled since the last report. Holding Shift goes slower (a tenth as far; the halo turns to ink, the puck takes a dot and the pointer a crosshair), taking Shift up or down mid-drag changes the speed from there without a jump. The arrow keys move one hundredth of the radius, Shift with an arrow a twentieth, Home, Delete and Backspace and a double-click put it back in the middle. The disc's size is whole physical pixels, so the ring is as thick on every side, and the puck and the crosshair are whole physical pixels too (an even number across), so at the middle the puck sits on the crosshair's centre at every scale. Under each wheel sits the master slider with its `lift`, `gamma`, `gain` or `offset` fill, and the wheels go two by two in the card (four across is too small to set a colour with, and three leave one alone).
- `AnchorFrame` (kind `segmented`): a `PictureSegmented` whose options are places in a frame instead of tiles in a row (a name tag's corner). A recessed frame holding the outline of a picture, with a dot at each anchor (`across` and `down` place option i from 0 to 1) and the picked one a bar with the thick outline in the card's colour and a muted inside. The name of an anchor is its tooltip. Click an anchor, Tab between them, Space or Enter picks, and an arrow goes where `moves` sends it (four numbers per option in the order left, right, up, down, -1 for nowhere).
- `TextArea` (kind `field`): the many-line `TextField`: a recessed `trough` box that wraps and scrolls to keep the cursor in sight, with the thick accent outline while it has the focus. Empty, `skeleton` draws dim lines of text instead of a sentence, and `placeholder` is what a screen reader hears.
- `Dial` (kind `dial`): a strip of ticks under a mark at the centre, for a value that moves in steps (ISO, shutter time). It sits in the card, never in a bubble. Every tick is a value of its own: the camera's third stops are the ticks and every third one (`per-label` 3, from `label-from`) is a full stop, taller and named, small, in the strip; the ticks between are not named. A fixed readout at the mark always says the current value, big, at one place: its text just changes, it never fades or crossfades, so it cannot flicker or go blank. The strip's names fade out as they come to the readout so they never lie over it. The readout is smaller while the dial is being moved (a drag held, wheel notches or keys coming) and swells when it is let go (about 200 ms after the last step), shrinking again quickly when it moves; the swell is a layer (`cache-rendering-hint`, only while scaled), because femtovg hints glyphs and rounds a text scale, so a font size or a plain scale moves in whole pixels with the same height for several frames, then a jump; at rest the readout is plain text, pixel sharp. A plain drag, a mouse notch of the wheel and the arrows step through every tick, a click on a tick beside the mark picks it, Shift with an arrow and Page Up and Page Down go a tenth of the ticks, Home and End the ends. On a `free` dial (the camera's, whose ranges are continuous) Shift while dragging or scrolling goes a tenth as far and between the ticks (`moved-between`, the nearer tick and how far from it) and a click on the readout types one (`typed` hands over the text as it was written; the owner puts the value in through `current`, `between` and `value-text`, which the readout shows between ticks, or counts up `rejected` and the numbers shake back). The mark is muted at rest, and full colour with the border lit while the dial is held. Its value is a place in `stops`; with no stops it shows a dash and keeps its height.

### Selection is the outline

Every picked thing is the thick outline in a hue over a muted inside: a segment, a tile, a toggle, and in Settings the picked tab of the column (the accent) and the picked search result. The filled violet is for the one filled button and a lit switch's glyph, and `tests/design.rs` fails on a card that uses `Theme.fill` (a step's done mark in the setup card is the one listed exception). Settings' All, On and Off is the kit's `Segmented`, and a plugin tile or row there sits on the card or the window, with no hairline round it and no second box inside a card.

Two or more switches that belong together are `ToggleTile`s, each its own tile with its own outline (Transforms' Mirror and Upside down, Cinema's four tools), never a joined pair of filled segments.

### Bars: raised squares and one rule for labels

The top bar's controls are one height (`Theme.u(18)`, a little taller than a card's row), all raised squares or raised buttons: the device picker, the devices button, Presets, Settings, Add camera, the MCP button (two raised segments side by side, outlined in the accent while an AI is changing things). A layout of their own centres each on the bar's height (a `y` inside a layout is ignored). The preview's chin follows the card rule: every control is an icon with its word in the tooltip, the actions (Hide, Pop out) are raised icon buttons and the ones that stay on (Camera, Skeleton, Lens outlines, Smooth) are icon `ToggleTile`s lit with the outline in the preview's colour. Smooth also shows its word (`natural`, `text`): people look for it by name, and the tile keeps its whole word at any width of the row. The window's footer is the top bar's colour: the streaming camera in ink and its size dim on the left (or the status in a word), and while streaming the frame rate and throughput on the right as readouts in a card's value type, each behind a small icon with its full name as the tooltip, in a box wide enough for its widest value. Nothing in it is a dot or a pill.

The OS title bar: on Windows 11 and on KDE Plasma the caption takes the top bar's colour (`Theme.chrome`) and its text the bar's ink, set again when the look changes (`frame::dress`, and `kde.rs` for Plasma). GNOME and the other desktops draw their own from the system.

### Icons

One drawing language: a 16 unit grid, a 1.4 stroke with round caps and joins, filled shapes only where the thing is solid (a stop, the contrast half). Every icon says what its row does to someone who hasn't seen it, and a new one is looked at on the contact sheet at 8x for gaps, bumps and uneven strokes before it goes in. The camera has one base drawing (`camera`, the Camera card's) and every other camera is an edit of it: `camera-off` is the base crossed out, `snapshot` is the base with a flash dot.

### Icon rail

A card's label column is a column of icons: the icon is the label and the full word is its tooltip, in every card (Settings' pages keep named rows). A slider on the rail is the effects crate's `RailSlider`, which marks itself changed by comparing with its default; a row of picture settings the camera reports (the Webcam card) takes its icon from the control's name. `RailRow` (kind `row`) is the one row for that: an icon (`icon`, in the card's colour quietened, grey when disabled), the word it stands for (`label`: the tooltip, what a screen reader says and what the settings search finds), an optional longer `tip`, then the control. A row that goes on from the one above has `has-icon: false` and keeps the rail's room blank.

One on/off setting gets a `SwitchRow` (kind `row`): the icon, the setting's name as text beside it (cut with an ellipsis and a tooltip when it doesn't fit) and the kit `Switch` out at the right edge, centred on the row. A lone switch row always carries its label, since the icon alone says little for an unusual setting. Under it is a `RailRow` with `named` set, which is the one option that writes the label out. A `ToggleTile` is only for a group of them (Camera's four, Phone's Notify and Stop pairs) or an icon-only tile in a row of buttons; never one on its own across a full row. `design.rs` fails on a `RailRow` that holds a lone `ToggleTile` or a lone `Switch`. A `ToggleTile` with no `icon` has no icon slot, its text starts at the tile's padding.

An icon that is part of a row says what the row says: in a `named` row (a plugin row, a `SwitchRow`) the icon, the name and the control all show the row's own tip (`tip`, `tip-detail`) and the icon takes none of its own; a card header's icon shows the title's tip. Only a bare icon that is the whole label (a `RailRow` that isn't `named`) has a tip of its own, its word. A `ProblemMark` is centred on its row's control, and its "!" is drawn (a stem and a dot, centred by their own height) rather than typeset, so the same room is above and below it inside the circle.

A setting changed from its default gets a quiet mark on its row, from `changed` on the `RailRow` (the card works it out from its own values). No reset icon: a double-click on a slider puts it back, and a card with a header Reset lights it when anything on the card is off its default. `Theme.mark-dot` picks how the mark looks, in one place for every card: false lights the row's icon (the card's colour mixed half towards the text colour), true leaves the icon quiet and puts a small dot in the card's colour at its corner. The specimen's rail row shows it in the "on" column.

## Bubbles

One popover for everything that needs one (the colour picker now; the live crop, the chasing dots and the like later). It is the kit's `Bubble`: a frame in the card's colour with a pointer at the control it belongs to, a `bubble` surface, `Theme.stroke` for its line and `r-bubble` for its corners. It sits inside the control it points at, as big as that control, and takes no room in the card, so opening it moves nothing. It opens above the control, flips below when it doesn't fit above, keeps its frame inside the window (`BubbleSpot` has the arithmetic) and puts the pointer over the control's middle. A press anywhere outside closes it and the card behind never sees that press; Escape closes it too. The focus goes in when it opens and back to the control when it closes. What goes inside is up to the caller, built from kit controls.

The frame and its pointer are one outline: a single closed path stroked at `Theme.stroke`, so the pointer has the frame's own line and joins it with no seam, wherever along the edge it sits. It pops in from `Motion.pop-from` with a hair of overshoot and fades in, and fades out when it closes (Escape closes it at once; a press outside eases it out), and the pop-up goes when that is over.

## Live pictures

A picture in a card or a bubble is a small copy that costs nothing until someone touches the control. The held bubbles below are the one use now. The Transforms card's zoom and pan map used to have one under its crop and doesn't: the phone only streams the crop and the main preview already shows it, so the map is the frame's shape on an empty field with the crop drawn on it (the pad's frame handle: a rounded outline, the surround dimmed round the same rounded cut).

### Held bubbles

The popup bubble takes the focus and the pointer (a press outside closes it), which a slider being dragged or keyed can't spare, so a bubble that explains a number is drawn another way: a `Peek` in the control, the one `Peeks` global and a `PeekLayer` at the top of the window (next to the tooltips' layer, in the main window's document). It draws the same `BubbleFrame` (frame, pointer, `BubbleSpot`'s placement) at one size, `PeekBox`, for every kind, so what is inside changes and the bubble doesn't. It points at the control's handle (the slider gives `Peek` the handle's place: `has-point`, `point-x`, `point-y`, `point-w`, `point-h`) and follows it while it fits; near a window edge the frame stays inside the margin and its pointer slides along the frame's edge to keep pointing at the handle (`BubbleSpot.tip`). It takes no press and no key, so the drag or key that opened it goes on, it sits over the card and takes no room in it, and a tooltip waits while it is up.

When there is one:

- Only while the control is touched: held with the pointer, or keys used on it (arrows, Page keys, Home, End) and for a moment after the last one (about a second), and only while it has the focus. Hovering alone shows the tooltip, never the bubble.
- Only when the main preview can't show the change right then, because the effect is off or because the setting is a behaviour over time. The card decides and passes `peek` (a kind) to the slider, or `""` for none. A grade on a stream that's on has none.

What it draws, by kind: `picture` (a small picture the plugin gives it through `set_ui_image`), `chase` (a ring and a dot from four numbers: where you are and where the framing is) and `text` (a sample at a size, with the phone's 360 dp screen as the scale).

The cost rule: a live picture in a bubble is a small, low-frame-rate copy (about 8 a second) that only exists while the bubble is up. The slider says `peek-changed(open)` when its bubble comes up and goes down, the card hands that to its plugin, and the plugin makes pictures, asks for people or moves dots only between the two. Closed, nothing runs. `app/tests/peek_cards.rs` and `crates/shell/tests/peek.rs` check it.

## Status, lists, dialogs and rows that come and go

- `Status` (kind `status`): what a card is doing as one coloured word or short phrase (`kind` quiet, ok, warn, err), one line, ellipsis, no surface; the sentence goes in `detail`, its tooltip. `Tag` is a small raised label (`r-ctl` corners, no pills) with the same `detail`.
- Every button has a surface at rest: `Button` (every role) and `IconButton` are raised squares; only an icon inside a field is `bare`. `Button.reserve` lists every text a label can be, so the button never changes width; `Button.dialog` gives an even floor for a dialog's button row (`DialogButtons`).
- `Card`: the header's one reset button (`has-reset`, `reset-lit`, `reset-label`: icon only, lit only while something is off its default), `idle` (fades to the disabled look and back), a card with nothing under its header is no fold (no hover, no cursor), `action-reserve` for a header action whose text changes.
- `ListRow` (kind `list-row`): a raised strip in a list, name and a dim line on the left, its buttons on the right; a dialog is a title, section headings with ListRows or controls, and a `DialogButtons` row, never a Card inside a Card.
- `SwitchRow.problem`: the round red "!" (`ProblemMark`) before the switch (a pixel above the middle), tooltip name and body from `problem-title` and `problem`. `SwitchRow.word` is a short dim word next to the switch, where a second line of text under the row would have said it: one word or two, never a sentence.
- `PictureField` (kind `picker`): a file a card takes (the picture behind you, a LUT) as a raised slot with its thumbnail or an empty glyph and its name; a file that can't be opened gets a `ProblemMark` with the reason. The state is in the control, never in a line of text beside it.
- `LensRow` (kind `segmented`): the lens tiles, up to five in a row. Each tile's width is its own spring and its drawing fades with it, so three lenses becoming two or five reflow with motion (the others slide, a new one grows, a lost one shrinks and keeps what it said while it goes), and a change mid-way turns round from where everything is. More lenses than five are rows of a `PictureSegmented` with the same `LensArt`.
- `Card.reset-shortcut` is the reset button's own shortcut (a right-click on the button offers it); the card keeps no copy of it.
- Settings rows share one width for the control at their right edge, `Theme.control-w`; a `ChoiceField` prefers it too, so one in a layout that gives it no width still has a real one.
- `Status` shows a tooltip with its whole text only when the text is cut short (and its `detail` when it has one). `LinkText` lifts its words one physical pixel like every other word the kit centres, so it sits on the line of what is beside it. A `Segmented` gives each name its own room while the row has it and shrinks every name's room by the same share when the row is too narrow, so it fits at any width a card gives it. A held bubble that changes what it draws (`Peek`) fades the new drawing in over the old.
- `Unfolding` in an `UnfoldGroup`: rows that appear with a state open and shut in height and opacity, both ways, turning round mid-way; never `if state: Row`.
- Menus: `MenuSurface` and `MenuItem` (kind `menu`) are the one menu look and motion.
- Sliders and pads with a default say "Double-click to reset" themselves, so `tip` is only the name; a first click waits for the double-click interval (`ClickWait`), a drag never does.

## Tooltips

Most cards will turn labels into icons and put the word in a tooltip, so a tooltip is how people learn the window. It is part of the kit's look, not a system popup: Slint's own `Tooltip` follows the pointer, waits a fixed 500 ms and can't be placed, so the kit draws its own. Don't write `Tooltip { }` in a card.

What it says, in up to four parts and only the first is needed:

- The name (`tip`): body size, semibold. Most tooltips are only this.
- A detail (`tip-detail`): a line or two in `small` and `ink2`, wrapping at about 36 characters (the box is at most 260px wide with its padding, `Theme.u(120)` of text). It explains, the name doesn't repeat itself. A text that reads "Name. Longer explanation" is split into the two.
- A hint (`tip-hint`): a last line in `tiny` and `faint`, smaller and dimmer than the detail, for what a gesture does ("Double-click to reset"). Every kit control that takes `tip-detail` takes `tip-hint` too.
- Keys: small keycaps on the right of the name. A control with a `shortcut` shows the keys of its saved shortcut by itself (`Shortcuts.key-chips`); `tip-keys` gives them by hand.

Look: the `sel` surface (the token for a tooltip), a 1px `line-hover` outline, `r-ctl` corners so the roundness setting moves it, `u(5)` by `u(3)` padding and a soft shadow (the software renderer the specimen uses doesn't draw shadows; the window does). Keycaps are `card` chips. All tokens, so Classic and a later look follow. A dot in the card's colour before the name is there (`hue` on `Tip` and `TipBody`) and the kit's controls don't use it: only the controls that are given the card's colour (switch, slider, segmented) could carry it, and a card whose buttons and fields have none would show half its tooltips tinted. A card that wants it passes `hue`.

Where and when: it sits below the control it explains, centred on it, `u(2)` away, and flips above when it would run off the bottom and there is more room above. It is held `u(4)` inside the window's sides and never follows the pointer (`TipSpot` has the arithmetic). Timing is meant to be quiet while someone is working and quick while they are reading. All the numbers are named properties of the `Tips` global, beside `delay`:

- The wait counts from when the pointer rests, not from when it came in. The pointer rests while it stays inside `rest-radius` (4px) of where it last left that circle; a move that leaves it restarts the wait and recentres the circle there, so a pixel or two of jitter is still a rest and a slow creep is not. The window's input watch (`input.rs`) tells `Tips.moved` where the pointer is, since Slint only gives a touch area its own pointer position.
- The first tip comes up after a rest of `delay` (500 ms). For `warm-for` (1 s) after a tip went down the next one needs only `warm-rest` (100 ms), so crossing a row of controls shows nothing until the pointer settles.
- A press, the start of a drag or the wheel anywhere ends the warm time (`Tips.cool`: a `Tip` does it from `press` and `wheel`, the input watch does it for a press or wheel on anything else). The next tip needs the whole `delay` again, and it stays cold until a tip has shown.
- For `quiet-for` (2.5 s) after any value is changed from the window (a slider, dial, switch, choice, field), every wait is `quiet-factor` (2.5) times as long, warm ones too. The window calls `Tips.changed` whenever a card or a settings section sends one of its callbacks to its plugin.
- A press or the wheel on a control takes its tooltip down until the pointer leaves it. The keyboard focus (after Tab or an arrow key) shows it at once, with no wait, so a tooltip is there for people who don't use a mouse.

A window that wants these hooks exports `Tips` from its document (the main window, Settings and plugin windows do); without it the tips still show, after the plain `delay` from when the pointer came in.

How a control has one:

- A kit control takes `tip` (and `tip-detail`, `tip-keys`). It holds a `Tip`: `usage := Tip { says: root.tip; hover: touch.has-hover; inert: !root.enabled; focused: ...; }`, calls `usage.press()` and `usage.wheel()` from its touch area, and sets `inert` when its own touch area is off, so a disabled control can still say why.
- A text or a box with no touch area of its own holds `Tip { says: ...; inert: true; }`, which watches the pointer over it. Put it first among its siblings so it never covers something clickable. `enabled` turns it off (an elided text only says itself when it is cut).
- A `Tip` goes in a control (not in a layout) and covers it, so that is what the tooltip points at. It isn't under an `if`: nothing tells a tooltip its control was removed, though the window takes a tooltip down within a second of its control going.
- Every window has a `TipLayer` as its last child, which draws the tooltip over everything, and the kit's pop-ups (`DialogFrame`, `Bubble`) hold one each since a pop-up covers the window's. Without a layer a tooltip simply doesn't show.

`tests/tooltips.rs` has the timing, the placement and the parts; the specimen's second sheet (`tips-new.png`, `tips-classic.png`) draws a name, a detail, keys, a long detail, a tint and a tooltip flipped above and held in from the edge.

## Colours

`ColourField` is the one way to pick a colour: a swatch as wide as its row that shows the colour itself, in a raised well so a colour near the card's own still reads as a control. A click opens a bubble with a saturation and brightness pad, a hue strip, a pipette that picks from the preview (the plugin starts that pick) and, behind a small switch, the exact values: hex on its own line with its `#`, then red, green and blue in three equal boxes (three digits fit). The values open and close with the bubble growing and shrinking round them and the fields fading in a touch behind (`Unfold` and `Fade`). Typed hex is read with or without the `#`, as 3 or 6 digits, or with an alpha on the end (4 or 8 digits) which is dropped; anything else reverts (`effects::shared::parse_hex`). The pad and strip hold full colour, not the muted look of a slider's fill, since the colour is what they are for. The plugin keeps hue, saturation and brightness as the pad left them so a grey or a black keeps its hue, and stores whatever it already stored.

## Specimen sheet

Every kind in every state, side by side, in the new look and in Classic. Rows are kinds and columns are rest, on, hover, focus, held, disabled, placeholder and a long text (what a German translation gives). A kind with no cell in a column has no such state. Read it as a picture: a kind whose hover or held cell looks like its rest cell is the usual fault, and so is a long text that is cut off or runs out of its row.

```
cargo run -p telescope-shell --bin shell-specimen -- OUT
```

in `rust/` (with `SLINT_BACKEND=testing`, so nothing opens a window) writes `OUT/new.png` and `OUT/classic.png`. The cells are drawn from `crates/shell/ui/specimen.slint` and the grid is laid out in `crates/shell/src/specimen.rs`: a new kind or a new state is a line in its `KINDS` table and a block in the .slint file. Regenerate it after changing a widget and look at what moved.

## Shot tests

The tests named `*_shots.rs` in `app/tests` and the film tests in `crates/shell/tests` (`kit_motion_film.rs`, `kit_r2_film.rs`, `kit_leftovers.rs`) draw pictures from the real kit and card sources with the software renderer and save PNGs when `CARD_SHOTS`, `MOTION_SHOTS` or `KIT2_SHOTS` names a folder; without it they pass and draw nothing to disk. They are snapshots in the sense of pictures, not in the sense of stored pictures: nothing is compared with an earlier PNG and none is committed, so a change in how a widget looks is found by regenerating the pictures and looking. What the same tests assert (sizes, where things are, how a motion moves frame by frame) is checked every run. Render them under `SLINT_BACKEND=testing` so no window opens.

## Adding or changing a control

Put it in `ui/widgets`, give it a `control-kind` from the table (add the kind here first if it is truly new), build it from tokens only, add it to the specimen sheet, and use it from the card. A card that needs a look the kit lacks asks for a new kind or a new variation of one, it doesn't draw its own.

## Motion

Everything that moves goes through `ui/widgets/motion.slint`, so how the app feels is tuned in one file. A motion is a spring that Slint steps itself (`easing: spring(bounce)`, `duration` is the spring's period). Give a spring a new target mid-move and it keeps its speed, so a retarget bends the movement and nothing restarts or fights. Springs only ask for frames while they move.

Two families. Spatial motions (a place, a size) may overshoot a little when they are small and fast; effects (colour, opacity) never do and are held inside 0 and 1.

| Motion | For | Spring |
|---|---|---|
| `Settle` | A handle, a strip or a height that something other than the person moved | 200 ms, no overshoot |
| `Flick` | A switch's handle | 260 ms spring, bounce 0.22 (about 2% overshoot, so it never touches the track's border): a short move takes at least 220 ms, so its first frame moves under 1.5 px and its peak is about 3 px a frame |
| `Worm` | The picked option's outline in a one-of choice (segments, picture tiles, corner picker, tabs, the settings nav) | the leading edge on a 110 ms critical spring, the trailing edge on a 280 ms spring with bounce 0.3 that starts at the same moment (no delay); on a grid the second axis is 1.3x longer and the first 0.9x |
| `Unfolding` | Rows that come and go with a state (the blur sliders after Blur) | the `Unfold` spring drives height and opacity together, both ways |
| `Page` | The contents of a tabbed or optioned place (Primaries, Curves, LUT; Off, Blur, Picture): the picked page and the one that goes share one period both ways, so their heights always add up to one page's and the card goes straight from one height to the other; they overlap and cross-fade (`PageGroup`) | `Motion.reflow-open`, both ways |
| `ClickWait` | How long a first click waits for a double-click (a reset) | 350 ms, shared by sliders and the pan map |
| `Pop` | A bubble's size coming in, from 0.9 | 150 ms, bounce 0.4 |
| `Tone` | Hover, press and held colour (0 rest, 1 lit) | 90 ms up, 140 ms down, press at once |
| `Fade` | An opacity: bubbles, tooltips | 120 ms in, 100 ms out |
| `Unfold` | A card or section opening and closing | 170 ms open, 130 ms shut |
| `Press` | A button's face going back after a click | 220 ms, in at once |
| `Arrival` | Content that took a placeholder's place: half solid in the first frame, then up to full | via `Fade` |

The lens row's outline is not a Worm element: its two edges are tile numbers on the worm's springs and where a tile number is on screen is read from the tiles each frame, because a spring that chases a place another spring is still moving only moves once that one has stopped (the outline would sit at the placeholder's place and then jump).

Worm: a one-of choice (`Segmented`, `PictureSegmented`, `AnchorFrame`, the settings nav, All/On/Off, colour grading's pages) draws its picked outline and muted inside as one `Worm` over the options' surfaces and under their text. A pick stretches it onto the new option: the edge on the side it travels to leads (stiff, quick, lands exactly) and the other trails a beat later and passes its place a little as it snaps in, so it takes the same time however far it goes. On a grid (the corner picker) it goes across first and down a beat later, so the path bows. A key moves it like a click does, the first place it is given is never travelled to, and a layout that resizes moves it with the layout (it glides while a pick is under way and until it has arrived: every pick sets that and arriving clears it, so a pick mid-flight, or back to where it came from, never snaps it). The outline is drawn above the tiles, the names and the drawings and the muted inside under them (`outline-only` and a `WormFill`), in that order always. `PictureSegmented` has `columns`: one control, one worm, arrows along rows and up and down columns, never a one-dimensional index. The lens row's tiles make room for each other on `Motion.reflow-open` and `reflow-close` (the pace of the Name tag's position move, slower than a fold's `unfold-*` so the eye can follow it), both ways. A multi-select (`ToggleTile`) has no worm: its outline and inside fade in and out together (`Motion.tile-in`, `tile-out`), since nothing travels between the tiles.

Other motions: a dropdown's list fades in with a small drop and fades out after a pick (a key puts it away at once); a button's click darkens its face at once and eases back, with its words sinking a pixel, so even a quick click is seen; a card's rows fade as it closes round them, so a clipped handle never shows as a stray arc, and a shut card has the same room above its title as below it (the bottom padding goes with the fold); a dial's stops fade in when they arrive after the dash that held their place.

Rules:
- The person always wins. Only a finger on a control puts it where it is going at once (`snap`: a dragged slider or pad or dial strip). Keys, clicks on a track, resets and a change from outside (Auto, a preset, the phone, an MCP client) all glide, and nothing a key does snaps a place or a size (a key may make a colour follow at once). A fold the person makes, by click or key, glides; state loaded from outside snaps.
- Every motion chains with no seam: a new target mid-move keeps the place and the speed, however often it comes (`tests/kit_hammer.rs` retargets each motion 60 times at random moments and checks it against the ideal chain of springs). Slint starts the new spring from the place the property last evaluated, so a control that moves a target from a callback reads the motion's place first (`xyz.sync()`), or every retarget loses up to a frame of the move. No `delay`, no `cubic-bezier` or `ease` curve on anything that moves, no `snap` that a key sets (`tests/design.rs` checks all three).
- Whatever is removed is removed after its out-motion: a dropdown's list (a pick, Escape, a click outside, a lost focus), a dialog (`DialogFrame.leave()`, `left`), a menu (`MenuSurface.leaving`), a bubble all fade out and the pop-up goes after; clicking the field while its list is going turns it round. A tooltip goes down with its owner (`Tips.dismiss()` for what hides with `visible`; the heartbeat for what is removed).
- A stepped slider (steps of 8 px or more) is a magnet while dragged: the handle follows the pointer, held back near each step (`detent`), the value changes at the midpoint between steps, and the handle springs into its slot when let go.
- A stroked `Path` is drawn 1:1: its element is the box, its viewbox the box less one stroke, so a unit is a pixel (`bubble.slint`, `pad.slint`, `stepper.slint`); positions that a handle moves are whole physical pixels (`round(x / 1phx) * 1phx`).
- Text centred in a box is lifted one physical pixel (`property <length> lift: 1phx;`), because Plex's capitals sit low in its line box; a drawing and a name in a tile are centred as one block from the drawing's top to the name's baseline.
- A move across and down leads with one axis (`Motion.lead`, `Motion.trail`), so the path bows and never runs straight or turns a corner.
- A colour that depends on state animates with `animate background { duration: Motion.tone-for(pressed); easing: Motion.tone; }`; a colour that comes up while held is a `Tone` and is mixed with `Theme.lit` or `mix`, which hold their factor between 0 and 1.
- Nothing runs longer than about 400 ms, and no animation changes a card's height except a fold, which is the height change.
- Reduced motion (the desktop's setting, `TELESCOPE_MOTION=full|reduced|off`, or the `Motion` global) snaps every move and keeps colour and opacity; off keeps nothing.
- Springs are animated on a built-in element number (a border width), not on a property of the component and not on an x: the interpreter the app runs the kit through only carries the speed over for built-in non-geometry properties.
- `transform-scale` (the bubble's pop) is drawn by the femtovg renderer and ignored by the software one, which the tests and renders use.
- A motion that is not a child of a layout: an empty element in a layout takes a cell, so put them inside a Rectangle.

