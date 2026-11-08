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

## Type

One family, a few styles, weight and size carrying the hierarchy (no capitals, no tracking tricks: some scripts have no case).

| Style | Size | Weight | Use |
|---|---|---|---|
| Title | `card-title` | 600 | Card titles |
| Value | `body` | 600 | A current value, a picked option, a button label |
| Label | `body` | 500 | Parameter names, option text |
| Section | `small` | 600 | A group's title inside a card |
| Unit | `tiny` | 500 | Units and captions beside a value |

Text that stays in a card is styled as something: a status, a badge, a key, an empty state. No loose explaining sentences.

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

A menu button is always a `picker`: the value and a chevron, never "..." in a label.

## Sliders

One anatomy for every slider: a track, a handle, the value on the right, and a tick at the centre when the slider goes both ways (a range that crosses zero, or `two-way`). Only the fill varies, from a fixed list:

- `plain`: the card's colour from the start of the track up to the handle.
- `gradient`: the range's own colours along the whole track (`grad-a`, `grad-b`, `grad-c`), the handle taking the colour under it. Colour grading's temperature, tint and saturation use this.

At rest everything is muted: the fill and the handle's border in the quietened colour, the handle's inside in the card's. While the slider is held its full colour comes up, handle included, and dims when it is let go. A simple colour change for now; the animations are a later pass.

## Adding or changing a control

Put it in `ui/widgets`, give it a `control-kind` from the table (add the kind here first if it is truly new), build it from tokens only, and use it from the card. A card that needs a look the kit lacks asks for a new kind or a new variation of one, it doesn't draw its own.
