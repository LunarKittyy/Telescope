<!-- covers: rust/crates/effects/src/fun_* rust/crates/effects/src/cinema* rust/crates/effects/src/mosh.rs rust/crates/effects/src/peek* rust/crates/effects/src/light_planes* rust/crates/effects/src/slit_planes* rust/crates/effects/src/qr_reader* rust/crates/effects/src/photos* rust/crates/effects/src/colour_grading* rust/crates/effects/src/cube.rs rust/crates/effects/src/filter* rust/crates/effects/src/notes* rust/crates/effects/src/stargazer* rust/crates/effects/src/name_tag* rust/crates/effects/src/tag* rust/crates/effects/src/scopes* rust/crates/effects/src/*.wgsl android/app/src/main/kotlin/com/telescope/Notes.kt android/app/src/main/kotlin/com/telescope/Tag.kt; checked: 0d35dc883 -->
# Effects and overlays

Fun effects, looks and things drawn on top of the picture.

## Fun pack

Optional, off until you switch it on under **Extras** in the Plugins tab. Its card has one **Effect** picker:

- **Clone party** copies you into the picture after a three second countdown. Move somewhere else and add another, up to eight. With the vision download your outline comes from MediaPipe, otherwise take the empty room first and the clones are cut out by how much the picture differs from it
- **Invisibility cloak** fills in the empty room wherever you are. Take the empty room, step back in and you're gone. A plain wall behind you works better than one the same colour as your clothes. Edges are cleaner with the vision download
- **Light painting** leaves trails behind anything bright. Wave a torch or a phone screen around in a dark room. **Trails** sets how long they hang around and **Brightness** how bright a light has to be to paint
- **Slit-scan** shows each band of the picture a little later than the one before, which stretches and bends anything that moves. **Spread** is how many frames the picture spans from one end to the other
- **Stick figure** draws you as a stick figure on the empty room, or on paper if you haven't taken one. Crossed legs no longer swap sides and in a close-up the head is sized to your shoulders and nothing is drawn below the picture. It needs the vision download
- **Game Boy**, **1998 webcam**, **VHS**, **CRT** and **ASCII** are retro looks with nothing to set. The 1998 webcam works on a picture 320 pixels wide and costs little processor even at 4K
- **Datamosh** drops keyframes from the video before it's decoded. The motion in the frames after them then smears over the old picture instead of refreshing it. **Smear** plays each frame's motion up to three extra times and **Clean up** lets one keyframe through to get a clear picture back. It only works on Light (Heavy sends whole pictures with no keyframes to drop) and not with Browser camera. Phones that refresh the picture gradually instead of sending keyframes heal a mosh within about a second and the effect is more of a flicker there

The empty room isn't saved and needs taking again after the phone moves. Switching the effect off or to another one gets the phone to send a keyframe and the picture is back right away.

## Cinema tools

Optional, for judging focus and exposure while you set ISO, shutter and focus by hand. It also has framing guides and a level for getting the horizon straight. Switch it on under **Preview tools** in the Plugins tab of Settings. Everything it draws is only on the preview and apps using the virtual camera never get any of it.

- **Focus peaking** outlines what's sharp in red (or green, blue or white). Turn the focus until the outline sits on the thing you care about
- **Zebras** stripe the parts of the picture that are about to clip to white. They start at 95% brightness and **Zebras from** under Video in Settings moves that between 70 and 100%
- **False colour** paints the picture by brightness: purple and blue for crushed and too dark, green for middle grey, pink for skin about a stop over it, yellow for nearly clipped and red for clipped. Everything well exposed in between turns grey. A small legend shows in the corner
- **Guides** draws thin lines over the preview to frame the shot by: **Thirds**, a **Centre cross**, a **4 × 4** grid or the **Golden ratio** (like thirds, with the lines at 38% and 62%)
- **Scope** puts a histogram, waveform, RGB parade or vectorscope in the bottom right of the preview. The waveform and parade show brightness from 0 at the bottom to 100% at the top, with a line every 25%. The vectorscope marks the colour bar targets and has a line for skin tones
- **Level** draws the horizon from the phone's motion sensors as a line between two fixed marks for level. Line it up with the marks and the guide turns green. The number next to it is how many degrees the picture is tilted and the one underneath is how far the camera looks up or down. It knows which camera is streaming and follows the flip and rotation under Picture. The phone only reads its motion sensors while the guide is on screen. Its app has to be up to date for it (an older one gets a note on the card instead)

Peaking colour and sensitivity and how big the scope is are under Video in Settings. The tools work on the preview's own smaller copy of the picture and the scopes are worked out on their own thread about 12 times a second, to avoid slowing the stream itself down.

## QR codes

Optional. Switch on the QR codes card and hold a code up to the camera to get it on the clipboard. A Wi-Fi code gives you its password (or the network's name when it has none), useful for joining a café's Wi-Fi on the computer. A link or any other text is copied as it is. Once the code has read steadily for a moment it gets a green outline on the preview and a notification shows what was copied (**Notify me when a code is read** turns that off).

Telescope looks about three times a second at a smaller copy away from the stream itself and the picture doesn't slow down for it. The same code isn't copied again until it's been out of view for a few seconds. On Linux with Wayland a window in the background isn't allowed to change the clipboard. When that happens the card shows a note about it and **Copy** puts the code there with one click.

## Photos

Optional. Switch it on under **Camera** in the Plugins tab of Settings and the Photos card takes a real photo on the phone, at the camera's full quality instead of a frame of the video. It uses the lens, zoom, focus and manual settings the video has at that moment and turns the photo the same way as the preview. The card only shows what the current lens can do. On a lot of phones that's JPEG and a burst, while a flagship gets everything below:

- **Format** is JPEG with a quality setting, HEIC, Ultra HDR (Android 14) or RAW as a DNG, alone or together with a JPEG
- **Full resolution** under **Mode** uses the sensor's real size (the 50 or 200 MP on sensors that normally give 12). **Night**, **HDR** and **Bokeh** are the phone maker's own modes and show up if the maker lets other apps use them (Android 12)
- **Shots** can be a burst of up to 60, an exposure bracket for HDR merging or a focus bracket from near to far for focus stacking
- **Merge focus** appears for a focus bracket saved as JPEG (or RAW and JPEG). It is **Photos** to save only the photos, **Both** to save one merged picture next to them, or **Merged** to save only the merged picture and delete the photos. The merged picture is built from the sharpest photo at each point with a soft blend between them. The photos are lined up first, including the small size change as the focus moves. It is a JPEG named after the first photo with `_focus-merged`. It takes a few seconds after the photos arrive. If the merge fails the photos are still saved
- **Exposure** takes a long exposure up to what the sensor allows (30 s or more on some phones) at the video's ISO
- There's a 3 or 10 second **Timer** and best quality noise reduction and sharpening, plus distortion correction that can be turned on or off where the phone allows it

Photos go to the folder under **Save in** (a Telescope folder in your Pictures by default) with the phone's file name. **Open folder** shows the last ones. A plain photo is taken next to the video, which hitches for a moment the first time only. Night, HDR, Bokeh, long exposures and full resolution on some phones pause the video for a few seconds instead and the stream picks up again afterwards without a reconnect. **Take photo** also works as a [keyboard shortcut](app.md#keyboard-shortcuts), even with Telescope in the background. A photo taken that way pops up a notification once it's saved.

## Colour grading

An optional plugin for the look of the picture, switched on in Plugins (it needs no download). Like Background it runs on the computer and works with any phone and with Browser camera. Each phone gets its own grade and grades are saved in presets.

**Primaries** has four colour wheels. Lift is the shadows, Gamma the midtones, Gain the highlights and Offset all of it. Drag a wheel towards a colour to push the picture that way and use the slider under it for the level. Below them are Temperature and Tint, Contrast with its Pivot, Saturation, Vibrance and then Shadows and Highlights for brightening or darkening only the darker or brighter parts. A double-click resets any wheel, slider or curve. With the keyboard the arrows move a wheel's dot and Home centres it.

**Auto** in the card's header sets Temperature, Tint, Gamma, Gain, Lift, Contrast, Highlights and Saturation from about a second of the picture. The stream has to be running for it. It uses the vision download to get natural skin tones on faces and asks for it the first time if it isn't there yet. You can still change the sliders afterwards. The button reads **Looks fine** when there was nothing to correct.

**Vibrance** is like Saturation but pushes the dull colours more than the strong ones and goes easy on skin tones. Drag it left to calm down the loudest colours.

**Curves** has the RGB curves (all channels together, or red, green and blue on their own) plus Hue vs sat and Hue vs hue. On those two you pull a hue up to give it more colour or turn it towards the next hue along. Pulling it down does the opposite. Click to add a point and drag it, or double-click a point to take it off. In a curve that has keyboard focus **[** and **]** pick a point, the arrows move it and Delete removes it.

**LUT** puts a `.cube` file on top of the grade (1D or 3D, up to 65 points per side) with a Strength slider. **Export grade…** saves the whole grade, LUT included, as a 33 or 65 point `.cube` to get the same look in other software.

**Compare** splits the preview with the picture before the grade left of the line. Apps always get the graded picture. The switch in the card's header turns the grade off without losing it. Each wheel, slider, the Compare button, the switch and the LUT strength can go on a key in Keyboard shortcuts.

## Filters

An optional plugin with the basic filters of a photo editor, switched on under **Look** in the Plugins tab. It needs no download, except for skin smoothing. A filter at 0 is off and costs nothing. The switch in the card's header sets them all aside without losing them. Each phone has its own settings and presets save them.

- **Denoise** smooths the noise in each frame without touching the edges. **Over time** also averages every part of the picture that holds still with the frames before it, which cleans up a dim room far better than blurring. Anything that moves is left out so it can't leave a ghost. It starts over when the camera or the picture size changes and after a pause while no app reads the camera
- **Dehaze** takes the washed-out look off a dirty lens, a window behind you or a cheap camera's low contrast
- **Skin smoothing** softens skin inside the person mask and leaves eyes, brows, hair and edges sharp. It needs the vision download and the card mentions it until it's there
- **Sharpen** has an **Amount**, a **Radius** and a **Threshold** that leaves flat areas like skin and walls alone. Strong edges get steeper without a white outline. **Clarity** adds contrast to the mid-sized detail to make a flat picture look less flat, with no glowing outlines. Below 0 it softens instead
- **Vignette** darkens the corners (or lightens them below 0) with a **Size** and a **Roundness**. **Glow** puts a soft bloom around the bright parts and **Film grain** adds grain that moves every frame and is strongest in the mid-tones

Denoise, dehaze, skin smoothing, clarity and sharpen come before Colour grading in the picture's path. The vignette, glow and grain come after it and the grade doesn't change them. They run on the graphics card and fall back to the processor while something like a game has the card busy. The Camera card has the phone's own **Sharpening** and **Noise reduction**. Use one or the other, since sharpening twice looks worse than once.

## Notes next to the lens

Optional, switched on under **On screen** in the Plugins tab of Settings. Type your notes on the Notes card and **Send to phone** puts them on the phone's screen in big text right under the front camera. Reading them there puts your eyes almost on the lens and whoever is watching sees you looking at them. It needs the front camera, the phone facing you and the phone app from the same release (with an older one the card asks you to update it).

- Each line you type is one step for **Next** and **Previous**. Both can go on a key in Keyboard shortcuts. A presenter clicker works too because it sends keys like a keyboard does
- **Auto** scrolls at the **Speed** you set once you press **Play**. **By hand** only moves with Next and Previous or when you drag the text on the phone
- **Text size** changes the size on the phone straight away. Edits to the text itself only show after you send again
- The card saves a few sets of notes, named after their first line
- On the phone the screen is on and black behind the text while the notes are up, to go easy on the battery with an OLED screen. Back closes them until you send again and stopping the stream takes them away

Android only lets an app open a screen while it's in front. With Telescope in the background the card asks you to open it on the phone and the notes appear once you do. With Preview open on the phone they wait until you close it, since opening them over it would make the stream stutter. Turned sideways in a holder, the text sits next to the lens instead of under it.

## Stargazer

An early version for a phone clipped to a telescope, in Lab. Switch it on under **Camera** in the Plugins tab of Settings. It stacks many frames into one sharper or cleaner picture and has a **Focus** readout that shows how sharp the picture is now and the best so far. Turn the focuser to the highest number.

- **Lucky imaging** is for the moon and planets. It collects **Frames** and stacks the **Sharpest** share of them
- **Live stack** adds every frame and leaves out what only shows once, like satellites and noise. It is for faint things
- **Star trails** takes the brightest value of one frame every few seconds (**Every**), which draws the stars as lines

**Start** begins a run and **Stop** ends it early. A lucky run stopped early still stacks what it has, if at least 3 frames made the cut. The finished stack is saved as a PNG in Pictures/Telescope/Stargazer and **Open folder** shows it. **Show the stack on the camera** sends it to the virtual camera in place of the live picture, refreshed about once a second.

A run belongs to one phone and one picture size. Switching phones or changing the resolution ends it and you get the stack so far. Collecting pauses while part of the picture is hidden, by Privacy blur or the wait screen. Lucky imaging holds at most 1.5 GB of frames and the card notes it when that lowers the count. A slow computer drops frames instead of using more memory.

## Name tag

Optional. It puts your name and pronouns in a corner of the picture, with a smaller second line under them if you want one (like your role). Switch it on under **On screen** in the Plugins tab of Settings and fill in the **Name tag** card. With several cameras each one has its own tag (or none), for example to leave your name off the desk camera.

- **Style** is **Bar** (a dark box with a coloured edge), **Pill**, **Light** or **Shadow**, which has no box at all and only a soft shadow behind the text. Each tile shows its look
- **Position** is the small frame next to the styles. Click the corner or the bottom centre you want. **Size** is measured against the picture's height and the tag looks the same at 720p and 4K
- Presets save the tag too. One preset can have it on for a talk and another can leave it off for a chat
- Names in any script work (Arabic and Hebrew ones start from the right) and anything too long for the picture is cut short with "…"
- The tag goes on the picture before Telescope fits it to the virtual camera's canvas. With a **Canvas size** of another shape it sits in the corner of the picture rather than of the canvas. A low-resolution stream on a bigger canvas makes its text a little soft too (a higher resolution on the Stream output card helps)
- **Mirror** on the Transforms card never mirrors the text. If your own preview in a video call shows it backwards, that's the call app mirroring your self view while everyone else sees it the right way round
