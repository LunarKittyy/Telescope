# Effects and overlays

Fun effects, looks, and things drawn on top of the picture.

## Fun pack

Optional, off until you switch it on under **Extras** in the Plugins tab. Its card has one **Effect** picker:

- **Clone party** copies you into the picture after a three second countdown. Move somewhere else and add another, up to eight. With the vision download your outline comes from MediaPipe, otherwise take the empty room first and the clones are cut out by how much the picture differs from it
- **Invisibility cloak** fills in the empty room wherever you are. Take the empty room, step back in and you're gone. A plain wall behind you works better than one the same colour as your clothes. Edges are cleaner with the vision download
- **Light painting** leaves trails behind anything bright. Wave a torch or a phone screen around in a dark room. **Trails** sets how long they hang around and **Brightness** how bright a light has to be to paint
- **Slit-scan** shows each band of the picture a little later than the one before, which stretches and bends anything that moves. **Spread** is how many frames the picture spans from one end to the other
- **Stick figure** draws you as a stick figure on the empty room, or on paper if you haven't taken one. It needs the vision download
- **Game Boy**, **1998 webcam**, **VHS**, **CRT** and **ASCII** are retro looks with nothing to set
- **Datamosh** drops keyframes from the video before it's decoded. The motion in the frames after them then smears over the old picture instead of refreshing it. **Smear** plays each frame's motion up to three extra times and **Clean up** lets one keyframe through to get a clear picture back. It only works on Light (Heavy sends whole pictures with no keyframes to drop) and not with Browser camera. Phones that refresh the picture gradually instead of sending keyframes heal a mosh within about a second, so it's more of a flicker there

The empty room isn't saved and needs taking again after the phone moves. Switching the effect off or to another one gets the phone to send a keyframe and the picture is back right away.

## Cinema tools

Optional, for judging focus and exposure while you set ISO, shutter and focus by hand. It also has framing guides and a level for getting the horizon straight. Switch it on under **Preview tools** in the Plugins tab of Settings. Everything it draws stays on the preview and apps using the virtual camera never get any of it.

- **Focus peaking** outlines what's sharp in red (or green, blue or white). Turn the focus until the outline sits on the thing you care about
- **Zebras** stripe the parts of the picture that are about to clip to white. They start at 95% brightness and **Zebras from** under Video in Settings moves that between 70 and 100%
- **False colour** paints the picture by brightness: purple and blue for crushed and too dark, green for middle grey, pink for skin about a stop over it, yellow for nearly clipped and red for clipped. Everything well exposed in between turns grey. A small legend shows in the corner
- **Guides** draws thin lines over the preview to frame the shot by: **Thirds**, a **Centre cross**, a **4 × 4** grid or the **Golden ratio** (like thirds, with the lines at 38% and 62%)
- **Scope** puts a histogram, waveform, RGB parade or vectorscope in the bottom right of the preview. The waveform and parade show brightness from 0 at the bottom to 100% at the top, with a line every 25%. The vectorscope marks the colour bar targets and has a line for skin tones
- **Level** draws the horizon from the phone's motion sensors as a line between two fixed marks for level. Line it up with the marks and the guide turns green. The number next to it is how many degrees the picture is tilted and the one underneath is how far the camera looks up or down. It knows which camera is streaming and follows the flip and rotation under Picture. The phone only reads its motion sensors while the guide is on screen. Its app has to be up to date for it (an older one gets a note on the card instead)

Peaking colour and sensitivity and how big the scope is are under Video in Settings. The tools work on the preview's own smaller copy of the picture and the scopes are worked out on their own thread about 12 times a second, which keeps the stream itself as fast as before.

## QR codes

Optional. Switch on the QR codes card and hold a code up to the camera to get it on the clipboard. A Wi-Fi code gives you its password (or the network's name when it has none), handy for joining a café's Wi-Fi on the computer. A link or any other text is copied as it is. Once the code has read steadily for a moment it gets a green outline on the preview, and a notification says what was copied (**Notify me when a code is read** turns that off).

Telescope looks about three times a second at a smaller copy away from the stream itself and the picture doesn't slow down for it. The same code isn't copied again until it's been out of view for a few seconds. On Linux with Wayland a window in the background isn't allowed to change the clipboard. When that happens the card says so and **Copy** puts the code there with one click.

## Photos

Optional. Switch it on under **Camera** in the Plugins tab of Settings and the Photos card takes a real photo on the phone, at the camera's full quality instead of a frame of the video. It uses the lens, zoom, focus and manual settings the video has at that moment and turns the photo the same way as the preview. The card only shows what the current lens can do. On a lot of phones that's JPEG and a burst, while a flagship gets everything below:

- **Format** is JPEG with a quality setting, HEIC, Ultra HDR (Android 14) or RAW as a DNG, alone or together with a JPEG
- **Full resolution** under **Mode** uses the sensor's real size (the 50 or 200 MP on sensors that normally give 12). **Night**, **HDR** and **Bokeh** are the phone maker's own modes and show up if the maker lets other apps use them (Android 12)
- **Shots** can be a burst of up to 60, an exposure bracket for HDR merging or a focus bracket from near to far for focus stacking
- **Exposure** takes a long exposure up to what the sensor allows (30 s or more on some phones) at the video's ISO
- There's a 3 or 10 second **Timer**, best quality noise reduction and sharpening, and distortion correction can be turned on or off where the phone allows it

Photos go to the folder under **Save in** (a Telescope folder in your Pictures by default) with the phone's file name. **Open folder** shows the last ones. A plain photo is taken next to the video, which hitches for a moment the first time only. Night, HDR, Bokeh, long exposures and full resolution on some phones pause the video for a few seconds instead and the stream picks up again afterwards without a reconnect. **Take photo** also works as a [keyboard shortcut](app.md#keyboard-shortcuts), even with Telescope in the background, and a photo taken that way pops up a notification once it's saved.

## Colour grading

An optional plugin for the look of the picture, switched on in Plugins (it needs no download). Like Background it runs on the computer and works with any phone and with Browser camera. Each phone gets its own grade and grades are saved in presets.

**Primaries** has four colour wheels. Lift is the shadows, Gamma the midtones, Gain the highlights and Offset all of it. Drag a wheel towards a colour to push the picture that way and use the slider under it for the level. Below them are Temperature and Tint, Contrast with its Pivot, Saturation, Vibrance and then Shadows and Highlights for brightening or darkening only the darker or brighter parts. A double-click puts any wheel, slider or curve back where it started. With the keyboard the arrows move a wheel's dot and Home centres it.

**Auto** in the card's header sets Temperature, Tint, Gamma, Gain, Lift, Contrast, Highlights and Saturation from about a second of the picture, so the stream has to be running. Faces keep natural skin tones, which is why it uses the vision download and asks for it the first time if it isn't there yet. The sliders stay yours to change afterwards, and the button says **Looks fine** when there was nothing to correct.

**Vibrance** is like Saturation but pushes the dull colours more than the strong ones and goes easy on skin tones. Drag it left to calm down the loudest colours.

**Curves** has the RGB curves (all channels together, or red, green and blue on their own) plus Hue vs sat and Hue vs hue. On those two you pull a hue up to give it more colour or turn it towards the next hue along. Pulling it down does the opposite. Click to add a point and drag it, or double-click a point to take it off. In a curve that has keyboard focus **[** and **]** pick a point, the arrows move it and Delete removes it.

**LUT** puts a `.cube` file on top of the grade (1D or 3D, up to 65 points per side) with a Strength slider. **Export grade…** saves the whole grade, LUT included, as a 33 or 65 point `.cube` to get the same look in other software.

**Compare** splits the preview with the picture before the grade left of the line. Apps always get the graded picture. The switch in the card's header turns the grade off without losing it. Each wheel, slider, the Compare button, the switch and the LUT strength can go on a key in Keyboard shortcuts.

## Filters

An optional plugin with the basic filters of a photo editor, switched on under **Look** in the Plugins tab. It needs no download, except for skin smoothing. A filter at 0 is off and costs nothing, and the switch in the card's header sets them all aside without losing them. Each phone has its own settings and presets save them.

- **Denoise** smooths the noise in each frame and keeps the edges. **Over time** also averages every part of the picture that holds still with the frames before it, which cleans up a dim room far better than blurring. Anything that moves is left out so it can't leave a ghost, and it starts over when the camera or the picture size changes
- **Dehaze** takes the washed-out look off a dirty lens, a window behind you or a cheap camera's low contrast
- **Skin smoothing** softens skin inside the person mask and keeps eyes, brows, hair and edges sharp. It needs the vision download, and the card says so until it's there
- **Sharpen** has an **Amount**, a **Radius** and a **Threshold** that leaves flat areas like skin and walls alone. Strong edges get steeper without a white outline. **Clarity** adds contrast to the mid-sized detail, which makes a flat picture look less flat, with no glowing outlines. Below 0 it softens instead
- **Vignette** darkens the corners (or lightens them below 0) with a **Size** and a **Roundness**. **Glow** puts a soft bloom around the bright parts and **Film grain** adds grain that moves every frame and is strongest in the mid-tones

Denoise, dehaze, skin smoothing, clarity and sharpen come before Colour grading in the picture's path and the vignette, glow and grain come after it, so the grade doesn't change them. They run on the graphics card and fall back to the processor while something like a game keeps the card busy. The Camera card has the phone's own **Sharpening** and **Noise reduction**, and sharpening twice looks worse than once, so use one or the other.

## Notes next to the lens

Optional, switched on under **On screen** in the Plugins tab of Settings. Type your notes on the Notes card and **Send to phone** puts them on the phone's screen in big text right under the front camera. Reading them there keeps your eyes almost on the lens and the people on the call see you looking at them. It needs the front camera and the phone facing you, and the phone app from the same release (with an older one the card asks you to update it).

- Each line you type is one step for **Next** and **Previous**. Both can go on a key in Keyboard shortcuts. A presenter clicker works too because it sends keys like a keyboard does
- **Auto** scrolls at the **Speed** you set once you press **Play**. **By hand** only moves with Next and Previous or when you drag the text on the phone
- **Text size** changes the size on the phone straight away. Edits to the text itself only show after you send again
- The card keeps a few sets of notes, named after their first line
- On the phone the screen stays on and black behind the text while the notes are up, which is easy on the battery with an OLED screen. Back closes them until you send again and stopping the stream takes them away

Android only lets an app open a screen while it's in front. With Telescope in the background the card asks you to open it on the phone and the notes appear once you do. With Preview open on the phone they wait until you close it, since opening them over it would make the stream stutter. Turned sideways in a holder, the text sits next to the lens instead of under it.

## Name tag

Optional. It puts your name and pronouns in a corner of the picture, with a smaller second line under them if you want one (like your role). Switch it on under **On screen** in the Plugins tab of Settings and fill in the **Name tag** card. With several cameras each one has its own tag (or none), so the desk camera doesn't have to show your name too.

- **Style** is **Bar** (a dark box with a coloured edge), **Pill**, **Light** or **Shadow**, which has no box at all and only a soft shadow behind the text. Each tile shows its look
- **Position** is the small frame next to the styles: click the corner or the bottom centre you want. **Size** is measured against the picture's height and the tag looks the same at 720p and 4K
- Presets save the tag too. One preset can have it on for a talk and another can leave it off for a chat
- Names in any script work (Arabic and Hebrew ones start from the right) and anything too long for the picture is cut short with "…"
- The tag goes on the picture before Telescope fits it to the virtual camera's canvas. With a **Canvas size** of another shape it sits in the corner of the picture rather than of the canvas. A low-resolution stream on a bigger canvas makes its text a little soft too (a higher resolution on the Stream output card helps)
- **Mirror** on the Transforms card never mirrors the text. If your own preview in a video call shows it backwards, that's the call app mirroring your self view while everyone else sees it the right way round
