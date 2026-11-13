# Framing and background

Keeping you in frame, and changing or hiding what is behind you.

## Auto framing

Optional. It finds you in the picture and moves zoom and pan to keep you framed. Switch it on under **People** in the Plugins tab of Settings (it needs [the vision download](#the-vision-download)).

- **Framing** sets how much of you is in the shot, from just your head to your whole body. Two people get framed together. With nobody in view for two seconds it zooms back out
- **Speed** goes from **Sloth** to **Zippy**. **Easy** is the default
- **Room to move** is how far you can shift before the framing follows (**Fidget** to **Roam**, while **Off** follows every move)
- **Show hands** (off by default) widens the view so your hands are in it too, for when you hold something out to the side
- Zoom and Pan on the Transforms card are greyed out while it's on. Flip and rotation still work and your own zoom and pan come back when you switch it off
- The skeleton button under the preview shows what the framing is reacting to. It never appears in the camera output

## Background

Optional and it shares the vision download. **Blur** blurs everything behind you and **Picture** puts an image there instead (PNG, JPEG, WebP or BMP). **Edges** sets how soft the outline around you is. It runs on the computer so it works with any phone and with Browser camera.

**High quality** swaps the cutout for a slower model (Robust Video Matting) that follows hair and edges much better and holds still where nothing moves. It's off by default because it uses more of the processor, about 40% of one core on a 1080p stream. It has a download of its own, about 25 MB on Linux and 20 MB on Windows, which the card offers when you switch it on. Until that's there, or if the model doesn't load, the normal cutout is used and the picture never waits for it. **Remove download** under People and vision on the Plugins tab deletes it as well.

## Expose for my face

Optional and it shares the vision download too. It moves the phone's metering point (the same one **Tap a point** on the Camera card uses) to your face, which helps with a bright window behind you. It moves the point slowly and leaves it alone for small head moves, since every move makes the phone meter and focus again. When you leave the picture the point stays where your face was instead of chasing the empty room.

Picking a point on the preview or a focus mode on the Camera card takes over from it until you click **Follow my face** or switch it off and on. It also waits while focus is on Manual (moving the point would undo that) until you click **Follow my face**. The phone only meters on the point while exposure is automatic. With manual exposure the point sets the focus and nothing else. On a front camera that can't focus on a point it only meters there and leaves the focus alone. A few lenses can't do either.

## Privacy blur

Optional. When more than one person is in the picture it hides all of it, in one of two ways picked on its card. **Blur the whole picture** blurs everything and **Show the wait screen** puts the wait screen up instead. It doesn't try to tell you from anyone else. Blurring starts as soon as one update from the people finder sees two or more people, and it only stops once there has been only one person (or nobody) for 4 seconds, so the finder losing a person for a moment doesn't make it flicker.

It only knows about the people MediaPipe finds. It can miss someone (a person turned away, small or half out of the frame), and it takes a moment to react. Don't count on it to keep anything private.

**Blur now** does the same by hand until you click it again and you can bind it to a key in Keyboard shortcuts. It works without the vision download too. Switching the card on for automatic blurring does need the download.

With several cameras streaming it watches the one shown in the panels, and whenever it blurs or covers that one the others get blurred all over too until it stops. A camera you clicked away from while it was blurred stays blurred.

Background and Auto framing work less while nothing in the picture moves. **Power-saving effects** under Video in Settings slows them down while you move too, which uses less of the processor and battery but follows quick moves a little later.

## Chroma key

Optional, for anyone with a green or blue screen. It swaps the screen behind you for a colour or a picture and needs no download. Next to Background it gives a sharper edge (hair included, as long as the screen is lit evenly) and uses a lot less of the computer. Switch it on under **Look** in the Plugins tab of Settings.

- **Screen** is **Green**, **Blue** or **Picked**. **Picked** asks you to click the screen on the preview and keys whatever colour is there, which helps with a screen that isn't quite the usual green or with uneven light
- **Tolerance** sets how many shades of the screen go (raise it for creases and shadows, lower it if bits of you vanish). **Edges** softens the outline and **Spill** takes the green or blue glow off your edges and skin without touching colours far from the screen's
- **Behind you** is a **Colour** you pick or a **Picture** (PNG, JPEG, WebP or BMP) scaled to cover the frame
- Presets save it like the other picture settings

## Desk and document mode

Optional. Point the phone down at a sheet of paper, mark the sheet's four corners on the preview and Telescope straightens it so calls see it flat and head-on (good for homework help, a sketch or signing something). Switch it on under **Camera** in the Plugins tab of Settings.

- **Mark the sheet** puts four corners on the preview. Drag each one onto a corner of the sheet (or drag the middle to move all four) and press Enter or **Done**. Esc or **Cancel** leaves things as they were. While you're marking, the preview shows the camera's real picture. Corners left bunched up or in a line aren't used. Mark the sheet again with one on each corner
- The corners are tied to the camera's picture. Zooming, flipping or rotating afterwards leaves them on the sheet, but moving the phone or the paper doesn't and then **Mark the sheet again** starts from where they were
- **Writing** can make the text stand out. **Contrast** evens out the light across the paper so it turns white and the ink stays dark. **Black and white** turns everything into pure black ink on white paper, which works best for printed text and pen
- Clicking the straightened picture still works the way you'd expect: **Tap a point** focuses on the word you clicked, scrolling zooms around it and other plugins' picks land where you clicked. The lens outlines and the skeleton aren't drawn while the sheet is straightened
- If zooming or Auto framing pushes part of the sheet off the picture the card warns about it. Zoom out or mark it again
- The sheet keeps its own shape, centred with dark bars beside it (white ones with **Contrast** or **Black and white**), so a standing page stays a standing page in a wide call. **Shape** set to **Auto** works the real shape out from the corners, undoing the tilt. **A4**, **Letter** and **Square** use that size instead, turned the way the sheet lies. **Fill** stretches it over the whole frame
- If the page still looks a bit squashed, drag the handle on the pad at the bottom of the card: across is the width, down is the height, from 50% to 200% (the corner of the drawn sheet follows it). A double-click on the pad or the reset button on it puts both back to 100%
- Each phone remembers its own corners and presets save them too

## The vision download

Auto framing, Background, Expose for my face and Privacy blur use MediaPipe to find people (the Fun pack can use it too). MediaPipe and its two models don't come with Telescope since most people won't use them. The first time you switch one of them on Telescope asks and downloads them from its GitHub releases (about 47 MB on Linux and 28 MB on Windows). They share the download and **Remove download** next to the People and vision heading on the Plugins tab deletes it again. Background's **High quality** option has a second download of its own (see [Background](#background)).

It only works on 64-bit Intel and AMD computers for now.
