<!-- covers: rust/crates/effects/src/framing* rust/crates/effects/src/auto_* rust/crates/effects/src/background* rust/crates/effects/src/face_exposure* rust/crates/effects/src/privacy_blur* rust/crates/effects/src/chroma_key* rust/crates/effects/src/desk* rust/crates/vision/src; checked: 0d35dc883 -->
# Framing and background

Keeping you in frame and changing or hiding what is behind you.

## Auto framing

Optional. It finds you in the picture and moves zoom and pan to follow you. Switch it on under **People** in the Plugins tab of Settings (it needs [the vision download](#the-vision-download)).

- **Framing** sets how much of you is in the shot, from only your head to your full body. Two people get framed together. With no person in view for two seconds it zooms back out
- **Speed** goes from **Sloth** to **Zippy**. **Easy** is the default
- **Room to move** is how far you can shift before the framing follows (**Fidget** to **Roam**, while **Off** follows every move)
- **Pick someone** lets you click one person on the preview and Auto framing follows only them. **Pick someone else** changes the choice and **Follow everyone** returns to the full group. When that person leaves the picture it zooms out after the usual moment and doesn't switch to another person. After 30 seconds without seeing them it releases them and frames all people again. The choice isn't saved and resets when Auto framing is switched off or the stream changes. People who walk past each other are told apart by the way each was heading. It drops the choice rather than guessing when it still can't tell who is who. With two phones paired for 3D a click on the combined picture can steer the wrong camera
- **Show hands** (off by default) widens the view so your hands are in it too, for when you hold something out to the side
- The crop runs on the graphics card when there is one (at 4K that's about 40% of a core less for the processor). While the card is busy with something else the processor does it and the picture looks the same
- Zoom and Pan on the Transforms card are greyed out while it's on. Flip and rotation still work and your own zoom and pan return when you switch it off
- The skeleton button under the preview shows what the framing is reacting to. It does not appear in the camera output

## Background

Optional and it shares the vision download. **Blur** blurs whatever is behind you and **Picture** puts an image there in place of it (PNG, JPEG, WebP or BMP). **Edges** sets how soft the outline around you is. It runs on the computer so it works with any phone and with Browser camera. The graphics card draws the blur, or lays the picture behind you, which takes load off the processor at 4K. While the card is busy the processor does it with the same picture. Pale clothes, chair backs and other large faint areas next to you are drawn solid, while thin edges and loose hair are left soft. **High quality** has its own cutout. Privacy blur doesn't use one and blurs the full picture.

**High quality** swaps the cutout for a slower model (Robust Video Matting) that follows hair and edges much better and holds still where there is no motion. It's off by default because it uses more of the processor, about 40% of one core on a 1080p stream. It has a separate download, about 25 MB on Linux and 20 MB on Windows, which the card offers when you switch it on. Until that's there, or if the model doesn't load, the normal cutout is used and the picture doesn't wait for it. **Remove download** under People on the Plugins tab deletes it as well.

The last cutout is still used on the current picture while a late one is waiting, including while settings change. Auto framing crops and scales that cutout with the picture while a fresh one is made. Background doesn't turn the picture black during the wait. Only the first cutout after Background turns on has no earlier cutout to reuse. Until it arrives all of the picture gets the blur or the picture, you included. Background is a comfort effect and doesn't protect private content or stop automatic photos from Phototrap or Stargazer.

## Expose for my face

Optional and it shares the vision download too. It moves the phone's metering point (the same one **Tap a point** on the Camera card uses) to your face, which helps with a bright window behind you. It moves the point slowly and leaves it alone for small head moves, since every move makes the phone meter and focus again. When you leave the picture the point is left where your face was rather than chasing the empty room.

Picking a point on the preview or a focus mode on the Camera card overrides it until you click **Follow my face** or switch it off and on. It also pauses while focus is on Manual (moving the point would undo that) until you click **Follow my face**. The phone only meters on the point while exposure is automatic. With manual exposure the point sets only the focus. On a front camera that can't focus on a point it only meters there and leaves the focus alone. A few lenses can't do either.

## Privacy blur

Optional and tagged **Limited** in the Plugins tab. It can't promise to hide every person because it can only blur someone once they have been found in the frame. When more than one person is in the picture it hides all of it, in one of two ways picked on its card. **Blur the whole picture** blurs all of it and **Show the wait screen** puts the wait screen up in place of the blur. It doesn't try to pick you out from another person. Blurring starts as soon as one update from the people finder sees two or more people. It only stops once there has been only one person (or none) for 4 seconds. That way the finder losing a person for a moment doesn't make it flicker. While the automatic blur is on the picture is still processed even when no app reads the camera, to catch someone walking in.

It reacts only to the people MediaPipe finds. It can miss someone (a person turned away, small or half out of the frame) and it takes a moment to react. Don't count on it to protect private content.

**Blur now** does the same manually until you click it again and you can bind it to a key in Keyboard shortcuts. It works without the vision download too. Switching the card on for automatic blurring does need the download.

With several cameras streaming it watches the one shown in the panels. Whenever it blurs or covers that one the others get blurred all over too until it stops. A camera you clicked away from while it was blurred is still blurred.

Background and Auto framing work less while no part of the picture moves. **Power-saving effects** under Video in Settings slows them down while you move too, which uses less of the processor and battery. A big move, like standing up or waving across the picture, still gets a fresh cutout straight away.

## Chroma key

Optional, for people with a green or blue screen. It swaps the screen behind you for a colour or a picture and needs no download. Next to Background it gives a sharper edge (hair included, as long as the screen is lit evenly) and uses a lot less of the computer. Switch it on under **Look** in the Plugins tab of Settings.

- **Screen** is **Green**, **Blue** or **Picked**. **Picked** asks you to click the screen on the preview and keys whatever colour is there, which helps with a screen that isn't exactly the usual green or with uneven light
- **Tolerance** sets how many shades of the screen go (raise it for creases and shadows, lower it if bits of you vanish). **Edges** softens the outline and **Spill** takes the green or blue glow off your edges and skin without touching colours far from the screen's
- **Behind you** is a **Colour** you pick or a **Picture** (PNG, JPEG, WebP or BMP) scaled to cover the frame
- If the graphics card fails while Chroma key is on, the picture goes dark until the card works again. It never sends the plain picture with the room showing
- Presets save it like the other picture settings

## Desk and document mode

Optional. Point the phone down at a sheet of paper, mark the sheet's four corners on the preview and Telescope straightens it so viewers see it flat and head-on (good for homework help, a sketch or signing something). Switch it on under **Camera** in the Plugins tab of Settings.

- **Mark the sheet** puts four corners on the preview. Drag each one onto a corner of the sheet (or drag the middle to move all four) and press Enter or **Done**. Esc or **Cancel** leaves things as they were. While you're marking, the preview shows the camera's real picture. Corners left bunched up or in a line aren't used. Mark the sheet again with one on each corner
- The corners are tied to the camera's picture. Zooming, flipping or rotating afterwards leaves them on the sheet but moving the phone or the paper doesn't (**Mark the sheet again** starts from where they were)
- **Writing** can make the text stand out. **Contrast** evens out the light across the paper so it turns white and the ink is still dark. **Black and white** turns it all into pure black ink on white paper, which works best for printed text and pen
- Both run on the graphics card when there is one (at 4K that's about 10 ms less work a frame for the processor with **Contrast**). While the card is busy with something else the processor does the cleanup and the page looks the same
- Clicking the straightened picture still works the way you'd expect: **Tap a point** focuses on the word you clicked, scrolling zooms around it and other plugins' picks land where you clicked. The lens outlines and the skeleton aren't drawn while the sheet is straightened
- If zooming or Auto framing pushes part of the sheet off the picture the card warns about it. Zoom out or mark it again
- The sheet is centred in its real proportions with dark bars beside it (white ones with **Contrast** or **Black and white**) and isn't stretched across a wide picture. **Shape** set to **Auto** works the real shape out from the corners, undoing the tilt. **A4**, **Letter** and **Square** use that size rather than the shape from the corners, turned the way the sheet lies. **Fill** stretches it over the full frame
- If the page still looks squashed, drag the handle on the pad at the bottom of the card. Across is the width, down is the height, from 50% to 200% (the corner of the drawn sheet follows it). A double-click on the pad or the reset button on it resets both to 100%
- Each phone remembers its own corners and presets save them too

## The vision download

Auto framing, Background, Expose for my face and Privacy blur use MediaPipe to find people (the Fun pack can use it too). MediaPipe and its two models don't come with Telescope since most people won't use them. The first time you switch one of them on Telescope shows a prompt and downloads them from its GitHub releases (about 47 MB on Linux and 28 MB on Windows). They share the download and **Remove download** next to the People heading on the Plugins tab deletes it again. Background's **High quality** option has a second download (see [Background](#background)).

It only works on 64-bit Intel and AMD computers for now.
