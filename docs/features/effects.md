<!-- covers: rust/crates/effects/src/fun_* rust/crates/effects/src/cinema* rust/crates/effects/src/mosh.rs rust/crates/effects/src/peek* rust/crates/effects/src/light_planes* rust/crates/effects/src/slit_planes* rust/crates/effects/src/qr_reader* rust/crates/effects/src/photos* rust/crates/effects/src/colour_grading* rust/crates/effects/src/cube.rs rust/crates/effects/src/filter* rust/crates/effects/src/notes* rust/crates/effects/src/stargazer* rust/crates/effects/src/name_tag* rust/crates/effects/src/tag* rust/crates/effects/src/scopes* rust/crates/effects/src/*.wgsl android/app/src/main/kotlin/com/telescope/Notes.kt android/app/src/main/kotlin/com/telescope/Tag.kt; checked: 88de924b5 -->
# Effects and overlays

Fun effects, looks and things drawn on top of the picture.

## Fun pack

Optional, off until you switch it on under **Extras** in the Plugins tab. Its card has one **Effect** picker:

- **Clone party** copies you into the picture after a three second countdown. Move somewhere else and add another, up to eight. With the vision download your outline comes from MediaPipe, otherwise take the empty room first and the clones are cut out by how much the picture differs from it
- **Invisibility cloak** fills in the empty room wherever you are. Take the empty room, step back in and you're gone. A plain wall behind you works better than one the same colour as your clothes. Edges are cleaner with the vision download
- **Light painting** leaves trails behind bright objects. Wave a torch or a phone screen around in a dark room. **Trails** sets how long they hang around and **Brightness** how bright a light has to be to paint
- **Slit-scan** shows each band of the picture a little later than the one before, which stretches and bends whatever moves. **Spread** is how many frames the picture spans from one end to the other
- **Stick figure** draws you as a stick figure on the empty room, or on paper if you haven't taken one. Crossed legs no longer swap sides and in a close-up the head is sized to your shoulders and no part of the figure is drawn below the picture. It needs the vision download
- **Game Boy**, **1998 webcam**, **VHS**, **CRT** and **ASCII** are retro looks with no settings. The 1998 webcam works on a picture 320 pixels wide and costs little processor even at 4K
- **Datamosh** drops keyframes from the video before it's decoded. The motion in the frames after them then smears over the old picture rather than refreshing it. **Smear** plays each frame's motion up to three extra times and **Clean up** lets one keyframe through to get a clear picture back. It only works on Light (Heavy sends complete pictures with no keyframes to drop) and not with Browser camera. Phones that refresh the picture gradually rather than sending keyframes heal a mosh within about a second and the effect is more of a flicker there

The empty room isn't saved and needs taking again after the phone moves. Switching the effect off or to another one gets the phone to send a keyframe and the picture is back right away.

## Cinema tools

Optional, for judging focus and exposure while you set ISO, shutter and focus manually. It also has framing guides and a level for getting the horizon straight. Switch it on under **Preview tools** in the Plugins tab of Settings. What it draws is only on the preview and apps using the virtual camera never get any of it.

- **Focus peaking** outlines what's sharp in red (or green, blue or white). Turn the focus until the outline sits on the thing you care about
- **Zebras** stripe the parts of the picture that are about to clip to white. They start at 95% brightness and **Zebras from** under Video in Settings moves that between 70 and 100%
- **False colour** paints the picture by brightness: purple and blue for crushed and too dark, green for middle grey, pink for skin about a stop over it, yellow for nearly clipped and red for clipped. Everything well exposed in between turns grey. A small legend shows in the corner
- **Guides** draws thin lines over the preview to frame the shot by: **Thirds**, a **Centre cross**, a **4 × 4** grid or the **Golden ratio** (like thirds, with the lines at 38% and 62%)
- **Scope** puts a histogram, waveform, RGB parade or vectorscope in the bottom right of the preview. The waveform and parade show brightness from 0 at the bottom to 100% at the top, with a line every 25%. The vectorscope marks the colour bar targets and has a line for skin tones
- **Level** draws the horizon from the phone's motion sensors as a line between two fixed marks for level. Line it up with the marks and the guide turns green. The number next to it is how many degrees the picture is tilted and the one underneath is how far the camera looks up or down. It matches whichever camera is streaming and follows the flip and rotation under Picture. The phone only reads its motion sensors while the guide is on screen. Its app has to be up to date for it (an older one gets a note on the card instead)

Peaking colour and sensitivity and how big the scope is are under Video in Settings. The tools work on the preview's own smaller copy of the picture and the scopes are worked out on their own thread about 12 times a second, to avoid slowing the stream itself down.

## QR codes

Optional. Switch on the QR codes card and hold a code up to the camera to get it on the clipboard. A Wi-Fi code gives you its password (or the network's name when it has none), useful for joining a café's Wi-Fi on the computer. A link or any other text is copied unchanged. Once the code has read steadily for a moment it gets a green outline on the preview and a notification shows what was copied (**Notify me when a code is read** turns that off).

Telescope looks about three times a second at a smaller copy away from the stream itself and the picture doesn't slow down for it. The same code isn't copied again until it's been out of view for a few seconds. On Linux with Wayland a window in the background isn't allowed to change the clipboard. When that happens the card shows a note about it and **Copy** puts the code there with one click.

## Photos

Optional. Switch it on under **Camera** in the Plugins tab of Settings and the Photos card takes a real photo on the phone, at the camera's full quality rather than a frame of the video. It uses the lens, focus and manual settings the video has at that moment. The card only shows what the current lens can do. On a lot of phones that's JPEG and a burst, while a flagship gets every option below:

- **Format** is JPEG with a quality setting, HEIC, Ultra HDR (Android 14) or RAW as a DNG, alone or together with a JPEG
- **Full resolution** under **Mode** uses the sensor's real size (the 50 or 200 MP on sensors that normally give 12). **Night**, **HDR** and **Bokeh** are the phone maker's own modes and show up if the maker allows other apps to use them (Android 12)
- **Shots** can be a burst of up to 60, an exposure bracket for HDR merging or a focus bracket from near to far for focus stacking. An exposure bracket has **Photos** (2 to 15) and **EV** (how far apart they are, from a third of a stop to 3), centred on the chosen exposure (or the video's exposure with **Video** selected). A focus bracket has **Steps** (2 to 100, in bigger jumps past 10) and a **Focus range** slider with a handle for the far end and one for the nearest. Both handles at the ends is the whole lens. A mark on the slider shows where the phone is focused now (on autofocus too with the latest phone app, otherwise only on manual focus). Focus on a point and drag a handle to the mark. A phone that has not been updated has the old limits (9 photos, 6 EV, 30 steps)
- **DSLRs over USB** take photos from this card on Linux and Windows, and the picture goes to the computer rather than the camera's card
- **Merge bracket** appears for a focus bracket or an exposure bracket saved as JPEG (or RAW and JPEG). **Photos** saves only the photos. **Both** adds one merged picture next to them. **Merged** saves only the merged picture and deletes the photos. A focus bracket is merged into a picture that is sharp from near to far, an exposure bracket into one with detail in both the dark and the bright parts. The photos are lined up first, including the small size change as the focus moves. Where something moved between the photos the middle photo is used. The merged picture is a JPEG named after the first photo with `_focus-merged` or `_exposure-merged`. It takes a few seconds after the photos arrive. A photo that can't be lined up is left out of the picture and still deleted with the others. If the merge fails the photos are still saved
- **Merged brightness** (exposure brackets only) leans the merged picture toward the darker photos or the lighter ones. The middle is the default
- **Exposure** takes a shutter speed from 1/4000 s up to what the sensor allows (30 s or more on some phones) in whole stops. **Customise camera** below Shots lets you override ISO for these photos. Choose **Camera** to use the camera's ISO. Long ones pause the video while the shutter is open
- **Focus** under **Customise camera** sets the focus for these photos, all the way left being infinity. Double-click the slider to go back to **Camera**. A dot shows where the phone is focused now and the handle sticks to it (on autofocus too with the latest phone app, otherwise only on manual focus). It's greyed out for a focus bracket, which sets its own focus, and on a phone without the latest app
- ISO and focus choices under **Customise camera** are saved with Photos settings and presets. The phone maker's modes set their own ISO and focus without erasing your choice
- There's a 3 or 10 second **Timer** and best quality noise reduction and sharpening, plus distortion correction that can be turned on or off where the phone allows it

**Mirror**, **Upside down** and rotation from Transforms always apply to non-RAW photos, including single photos, bursts, Night and Phototrap. JPEG photos have their blocks flipped or rotated without re-encoding. Partial blocks at the edges are trimmed where the turn needs it, like `jpegtran -trim`. HEIC and Ultra HDR photos aren't re-encoded either. A HEIC gets new rotation and mirror properties. An Ultra HDR photo gets a new EXIF orientation so its gain map still matches. RAW files are left untouched and no extra original is saved.

**Use zoom** on each photo card is off by default. Switch it on to crop to the manual zoom and position from Transforms, rounded to the JPEG MCU block grid. It is greyed out for HEIC and Ultra HDR, which get turned but not cropped. Pick JPEG to use it. Stretch, the Lens slider and Auto framing do not apply to photos. If zoom switched the preview to a telephoto lens, the photo uses that lens too. If a phone maker's mode can't use the zoomed lens, Telescope reports an error.

Each phone has its own Photos settings, including its shot settings.

Photos go to the folder under **Save in** (a Telescope folder in your Pictures by default) with the phone's file name. **Open folder** shows the last ones. A plain photo is taken next to the video, which hitches for a moment the first time only. Night, HDR, Bokeh, long exposures and full resolution on some phones pause the video for a few seconds instead and the stream picks up again afterwards without a reconnect. **Take photo** also works as a [keyboard shortcut](app.md#keyboard-shortcuts), even with Telescope in the background. A photo taken that way pops up a notification once it's saved.

A photo that pauses the video puts **Taking a picture** over the preview, with the shot count and an estimated time left. The popped-out preview shows it too and it clears when the photo finishes. The virtual camera continues to show its last picture without the message. The countdown runs down to the last second and the bar fills with it. A photo that takes longer than the estimate shows no countdown until it's done. With a self timer the message counts the timer down first. After a photo that pauses the video, the preview pauses briefly until the camera's exposure has settled again, at most about 1.5 s.

On a DSLR the Photos card has its own ISO and Shutter dials. What you set there and on the Webcam card is left on the camera after a photo. Shutter and ISO overrides from Stargazer or Phototrap are undone before live view returns, also after a cancelled or failed photo. A failed undo shows in the result and a warning. Cleanup can finish after a timeout. Check the camera settings after unplugging it during a photo.

## Phototrap

Optional. Draw up to four zones on the preview and arm it. Each time something moves in a zone the phone takes a short burst of photos. Nothing is saved until then and it never records video.

Each phone has its own Phototrap zones and settings.

Open **Customise camera** between **Pause** and **Night** to choose a shutter speed and ISO for unattended photos. **Camera** uses the camera's own value. These choices are saved with Phototrap settings and presets. **Focus** sets the focus for these photos, all the way left being infinity, and needs a phone with the latest app. When the phone maker's Night mode is available and switched on, it sets its own exposure and focus and these controls are disabled without erasing your choices. **Use zoom** crops these photos to the manual zoom and position from Transforms and is saved in presets with the other settings.

## Colour grading

An optional plugin for the look of the picture, switched on in Plugins (it needs no download). Like Background it runs on the computer and works with any phone and with Browser camera. Each phone gets its own grade and grades are saved in presets.

**Primaries** has four colour wheels. Lift is the shadows, Gamma the midtones, Gain the highlights and Offset all of it. Drag a wheel towards a colour to push the picture that way and use the slider under it for the level. Below them are Temperature and Tint, Contrast with its Pivot, Saturation, Vibrance and then Shadows and Highlights for brightening or darkening only the darker or brighter parts. A double-click resets any wheel, slider or curve. With the keyboard the arrows move a wheel's dot and Home centres it.

**Auto** in the card's header sets Temperature, Tint, Gamma, Gain, Lift, Contrast, Highlights and Saturation from about a second of the picture. The stream has to be running for it. It uses the vision download to get natural skin tones on faces and asks for it the first time if it isn't there yet. You can still change the sliders afterwards. The button reads **Looks fine** when no correction was needed.

**Vibrance** is like Saturation but pushes the dull colours more than the strong ones and goes easy on skin tones. Drag it left to calm down the loudest colours.

**Curves** has the RGB curves (all channels together, or red, green and blue on their own) plus Hue vs sat and Hue vs hue. On those two you pull a hue up to give it more colour or turn it towards the next hue along. Pulling it down does the opposite. Click to add a point and drag it, or double-click a point to take it off. In a curve that has keyboard focus **[** and **]** pick a point, the arrows move it and Delete removes it.

**LUT** puts a `.cube` file on top of the grade (1D or 3D, up to 65 points per side) with a Strength slider. **Export grade…** saves the whole grade, LUT included, as a 33 or 65 point `.cube` to get the same look in other software.

**Compare** splits the preview with the picture before the grade left of the line. Apps always get the graded picture. The switch in the card's header turns the grade off without losing it. Each wheel, slider, the Compare button, the switch and the LUT strength can go on a key in Keyboard shortcuts.

## Filters

An optional plugin with the basic filters of a photo editor, switched on under **Look** in the Plugins tab. It needs no download, except for skin smoothing. A filter at 0 is off and costs nothing. The switch in the card's header sets them all aside without losing them. Each phone has its own settings and presets save them.

- **Denoise** smooths the noise in each frame without touching the edges. **Over time** also averages every part of the picture that doesn't move with the frames before it, which cleans up a dim room far better than blurring. Whatever moves is left out so it can't leave a ghost. It starts over when the camera or the picture size changes and after a pause while no app reads the camera
- **Dehaze** takes the washed-out look off a dirty lens, a window behind you or a cheap camera's low contrast
- **Skin smoothing** softens skin inside the person mask and leaves eyes, brows, hair and edges sharp. It needs the vision download and the card mentions it until it's there
- **Sharpen** has an **Amount**, a **Radius** and a **Threshold** that leaves flat areas like skin and walls alone. Strong edges get steeper without a white outline. **Clarity** adds contrast to the mid-sized detail to make a flat picture look less flat, with no glowing outlines. Below 0 it softens rather than sharpens
- **Vignette** darkens the corners (or lightens them below 0) with a **Size** and a **Roundness**. **Glow** puts a soft bloom around the bright parts and **Film grain** adds grain that moves every frame and is strongest in the mid-tones

Denoise, dehaze, skin smoothing, clarity and sharpen come before Colour grading in the picture's path. The vignette, glow and grain come after it and the grade doesn't change them. They run on the graphics card and fall back to the processor while something like a game has the card busy. The Camera card has the phone's own **Sharpening** and **Noise reduction**. Use one or the other, since sharpening twice looks worse than once.

## Notes next to the lens

Optional, switched on under **On screen** in the Plugins tab of Settings. Type your notes on the Notes card and **Send to phone** puts them on the phone's screen in big text right under the front camera. Reading them there puts your eyes almost on the lens and whoever is watching sees you looking at them. It needs the front camera, the phone facing you and the phone app from the same release (with an older one the card shows a prompt to update it).

- Each line you type is one step for **Next** and **Previous**. Both can go on a key in Keyboard shortcuts. A presenter clicker works too because it sends keys like a keyboard does
- **Auto** scrolls at the **Speed** you set once you press **Play**. **By hand** only moves with Next and Previous or when you drag the text on the phone
- **Text size** changes the size on the phone straight away. Edits to the text itself only show after you send again
- The card saves a few sets of notes, named after their first line
- On the phone the screen is on and black behind the text while the notes are up, to go easy on the battery with an OLED screen. Back closes them until you send again and stopping the stream takes them away

Android only lets an app open a screen while it's in front. With Telescope in the background the card prompts you to open it on the phone and the notes appear once you do. With Preview open on the phone they wait until you close it, since opening them over it would make the stream stutter. Turned sideways in a holder, the text sits next to the lens rather than under it.

## Stargazer

An early version for a phone clipped to a telescope, in Lab. Switch it on under **Camera** in the Plugins tab of Settings. It stacks many frames into one sharper or cleaner picture and has a **Focus** readout that shows how sharp the picture is now and the best so far. Turn the focuser to the highest number.

Each phone has its own Stargazer settings. The photo settings for one camera don't carry over to another.

- **Lucky imaging** is for the moon and planets. It collects **Frames** and stacks the **Sharpest** share of them
- **Live stack** adds every frame and leaves out what only shows once, like satellites and noise. It is for faint things
- **Star trails** takes the brightest value of one frame every few seconds (**Every**), which draws the stars as lines

**Frames from** is under the three modes. It picks where the frames come from, **Video** or **Photos**. Photos work with Live stack and Star trails. Lucky imaging needs hundreds of frames and a photo takes seconds. It is video only. **Photos** is greyed out while Lucky is picked. Picking Lucky switches the frames to the video.

With **Photos** the camera takes a photo, Stargazer stacks it, then the camera takes the next one. A phone or a DSLR works. The video isn't used for the stack while it runs.

- **Photos** is how many to take before the run ends. **Every** is the shortest time from the start of one photo to the start of the next. **No wait** takes the next one as soon as the last is stacked. A photo that takes longer than **Every** is followed by the next at once. Telescope takes one at a time and does not take extra ones to catch up
- Open **Customise camera** to pick a shutter speed and ISO. Choose **Camera** on a dial to use the camera's own value. The first time you pick Photos it starts at 10 s and ISO 800, held to what the camera can do. A dial the camera can't set starts on Camera instead. A DSLR uses the nearest numeric choice it offers and restores the previous shutter and ISO after the shot. **Focus** sets the focus for the photos, all the way left being infinity. Choose **Camera** (double-click the slider) to use the camera's own focus. It needs a phone with the latest app. On an older phone or a DSLR it's greyed out and the photos use the camera's focus. The video pauses during a long exposure and the card shows when it is exposing
- **Use zoom** crops the photos to the manual zoom and position from Transforms
- The photos are saved as JPEG files in Pictures/Telescope/Stargazer next to the stack and remain after the run. A photo taken while the picture was hidden is deleted. The run stops when less than 1 GB is free on the drive. If a DSLR can't restore its shutter speed and ISO after a photo, the photo is still stacked and the card prompts you to check them. A photo that can't be read or taken ends the run with the stack you have, which **Save** still saves. A photo the stack can't line up with the others is counted as refused and the run goes on. The first photo of a stack is always taken, even a dark one with nothing to line up on
- Photos are stacked at one size for the whole run and the card shows that size. A very large photo such as one of 50 megapixels is stacked at half its size or less. Telescope works the size out from the photo's header before it reads the file. It counts the file, the JPEG decoder's working memory, the stack and the copy for the camera against 1.5 GB. That is an estimate with some margin and not a hard limit. The run stops before the photo is read if it can't be made to fit
- **Show the stack on the camera** puts a copy of the stack on the virtual camera, fitted to the video's picture size with black bars where the shapes differ
- No photo is taken or stacked while part of the picture is hidden. The run pauses and retries each second. It starts a photo no sooner than **Every** after the last one began. The card shows that it is paused

Switching **Frames from** or the mode starts from an empty stack. A saved **Night** shot from an older version is now Live stack with Photos, **Photos** set to 1 and its shutter speed, ISO and zoom as they were.

**Start** begins a run and **Stop** ends it early. **Stop** cancels a photo on its way and drops its answer if it arrives late. Everything stacked so far is yours to use. Star trails are saved and a live stack waits for **Save**. A DSLR may finish an exposure already in progress before restoring its settings. A lucky run stopped early still stacks what it has, if at least 3 frames made the cut. The finished stack is saved as a PNG in Pictures/Telescope/Stargazer and **Open folder** shows it. **Show the stack on the camera** sends it to the virtual camera in place of the live picture, refreshed about once a second. **Reset settings** in the card's header returns **Frames**, **Sharpest**, **Every**, **Photos** and the photo settings to their defaults when no run is going. The mode, **Frames from** and the camera switch aren't reset. Presets save all of these settings. Recalling one does not start a run or changes one that is going.

While a run collects video frames on Light, a phone with the latest app sends sharper video. Its bitrate climbs as far as the link allows (up to 100 Mbps) over the first 10 to 25 seconds and drops to your Stream output setting when the run ends. The video carries on without a pause. A stack of video frames belongs to one phone and one picture size. Switching phones or changing the resolution ends it and you get the stack so far. Collecting pauses while part of the picture is hidden, by Privacy blur or the wait screen. Lucky imaging holds at most 1.5 GB of frames and the card notes it when that lowers the count. A slow computer drops frames instead of using more memory.

## Name tag

Optional. It puts your name and pronouns in a corner of the picture, with a smaller second line under them if you want one (like your role). Switch it on under **On screen** in the Plugins tab of Settings and fill in the **Name tag** card. With several cameras each one has its own tag (or none), for example to leave your name off the desk camera.

- **Style** is **Bar** (a dark box with a coloured edge), **Pill**, **Light** or **Shadow**, which has no box at all and only a soft shadow behind the text. Each tile shows its look
- **Position** is the small frame next to the styles. Click the corner or the bottom centre you want. **Size** is measured against the picture's height and the tag looks the same at 720p and 4K
- Presets save the tag too. One preset can have it on for a talk and another can leave it off for a chat
- Names in any script work (Arabic and Hebrew ones start from the right) and text too long for the picture is cut short with "…"
- The tag goes on the picture before Telescope fits it to the virtual camera's canvas. With a **Canvas size** of another shape it sits in the corner of the picture rather than of the canvas. A low-resolution stream on a bigger canvas makes its text a little soft too (a higher resolution on the Stream output card helps)
- **Mirror** on the Transforms card does not mirror the text. If your own preview in a video call shows it backwards, that's the call app mirroring your self view while other people see it the right way round
