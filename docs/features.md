# Features

Jump to: [Camera](#camera-control) · [Picture](#stream-transforms) · [Auto framing](#auto-framing) · [Fun pack](#fun-pack) · [Chroma key](#chroma-key) · [Cinema tools](#cinema-tools) · [Name tag](#name-tag) · [Notes](#notes-next-to-the-lens) · [Desk mode](#desk-and-document-mode) · [Quality](#resolution-and-fps) · [Microphone](#microphone) · [Browser camera](#browser-camera) · [Several cameras](#several-cameras-at-once) · [On Air](#on-air-sign) · [Connection](#phones-pairing-and-connection) · [Privacy](#privacy) · [Updates](#updates) · [Automation](#system-integration) · [Shortcuts](#keyboard-shortcuts) · [Plugins](#plugins)

## Camera control

- **Lens picker** switches between the phone's real wide, main and telephoto sensors instead of faking it with digital zoom. On **Auto** the phone picks the lens as you zoom
- **ISO and shutter speed** by slider or typed value. The range follows the current lens
- **Brightness** (exposure compensation), usually ±8 EV
- **White balance** in Kelvin (2000-10000 K) with a green-magenta tint
- **Focus** is **Auto**, **Tap a point** (click the preview to focus and meter exposure there) or **Manual**
- **Stabilization**, noise reduction, sharpening, black level lock and the torch, on lenses that have them
- **Low light** lets auto exposure slow the camera down (usually to 10 fps at the lowest) so each frame catches more light. It only does that when it has to
- Anything the current lens can't do is greyed out. Hover **Full manual control** or **Limited manual control** to see what it supports

## Stream transforms

These apply live without restarting the camera.

- Flip horizontally or vertically
- Rotate 90° CW, 180° or 90° CCW
- **Zoom 1-10x** with pan. **Max zoom** under Video in Settings goes up to 30x. Scroll over the preview to zoom and drag to pan
- **Width** and **Height** stretch or squish the picture from 50% to 200%, each on its own. Whatever goes past the edge is cut off and a squish leaves black bars, like a rotated picture gets
- Double-click any slider to reset it. The pan pad takes arrow keys (Shift for bigger steps) and Home to centre

Zooming crops from the full sensor first and hops to the telephoto lens once your framing fits inside it, which is why zooming in gets sharper. Dots on the zoom slider mark where a longer lens takes over.

## Auto framing

Optional. It finds you in the picture and moves zoom and pan to keep you framed. Switch it on under **Optional** in the Plugins tab of Settings (it needs [the vision download](#the-vision-download)).

- **Framing** sets how much of you is in the shot, from just your head to your whole body. Two people get framed together. With nobody in view for two seconds it zooms back out
- **Speed** goes from **Sloth** to **Zippy**. **Easy** is the default
- **Room to move** is how far you can shift before the framing follows (**Fidget** to **Roam**, while **Off** follows every move)
- **Widen for something in your hand** (off by default) makes room when you hold something out to the side
- Zoom and Pan on the Transforms card are greyed out while it's on. Flip and rotation still work and your own zoom and pan come back when you switch it off
- The skeleton button under the preview shows what the framing is reacting to. It never appears in the camera output

## Background

Optional and it shares the vision download. **Blur** blurs everything behind you and **Picture** puts an image there instead (PNG, JPEG, WebP or BMP). **Edges** sets how soft the outline around you is. It runs on the computer so it works with any phone and with Browser camera.

## Expose for my face

Optional and it shares the vision download too. It moves the phone's metering point (the same one **Tap a point** on the Camera card uses) to your face, which helps with a bright window behind you. It moves the point slowly and leaves it alone for small head moves, since every move makes the phone meter and focus again. When you leave the picture the point stays where your face was instead of chasing the empty room.

Picking a point on the preview or a focus mode on the Camera card takes over from it until you click **Follow my face** or switch it off and on. It also waits while focus is on Manual (moving the point would undo that) until you click **Follow my face**. The phone only meters on the point while exposure is automatic. With manual exposure the point sets the focus and nothing else. On a front camera that can't focus on a point it only meters there and leaves the focus alone. A few lenses can't do either.

## Privacy blur

Optional. It hides other people who walk into the picture, in one of three ways picked on its card. **Blur the others** blurs everyone but you, **Blur all but me** blurs the room as well and **Show the wait screen** puts the wait screen up in place of the whole picture. "You" is whoever has been in the picture longest when blurring starts. If you step out while it blurs, nobody else gets taken for you: they stay blurred until you come back to about where you were. Blurring starts once a second person has been found a couple of times in a row and only stops after there has been only one person (or nobody) for about five seconds. Someone walking past or the model losing track for a moment won't make it flicker.

**Blur now** does the same by hand until you click it again and you can bind it to a key in Keyboard shortcuts. It works without the vision download too, in which case it blurs the whole picture since there's nobody to tell apart. Switching the card on for automatic blurring does need the download.

Background and Auto framing work less while nothing in the picture moves. **Power-saving effects** under Video in Settings slows them down while you move too, which uses less of the processor and battery but follows quick moves a little later.

## Fun pack

Optional, off until you switch it on in the Plugins tab. Its card has one **Effect** picker:

- **Clone party** copies you into the picture after a three second countdown. Move somewhere else and add another, up to eight. With the vision download your outline comes from MediaPipe, otherwise take the empty room first and the clones are cut out by how much the picture differs from it
- **Invisibility cloak** fills in the empty room wherever you are. Take the empty room, step back in and you're gone. A plain wall behind you works better than one the same colour as your clothes. Edges are cleaner with the vision download
- **Light painting** leaves trails behind anything bright. Wave a torch or a phone screen around in a dark room. **Trails** sets how long they hang around and **Brightness** how bright a light has to be to paint
- **Slit-scan** shows each band of the picture a little later than the one before, which stretches and bends anything that moves. **Spread** is how many frames the picture spans from one end to the other
- **Stick figure** draws you as a stick figure on the empty room, or on paper if you haven't taken one. It needs the vision download
- **Game Boy**, **1998 webcam**, **VHS**, **CRT** and **ASCII** are retro looks with nothing to set
- **Datamosh** drops keyframes from the video before it's decoded. The motion in the frames after them then smears over the old picture instead of refreshing it. **Smear** plays each frame's motion up to three extra times and **Clean up** lets one keyframe through to get a clear picture back. It only works on Light (Heavy sends whole pictures with no keyframes to drop) and not with Browser camera. Phones that refresh the picture gradually instead of sending keyframes heal a mosh within about a second, so it's more of a flicker there

The empty room isn't saved and needs taking again after the phone moves. Switching the effect off or to another one gets the phone to send a keyframe and the picture is back right away.

## Chroma key

Optional, for anyone with a green or blue screen. It swaps the screen behind you for a colour or a picture and needs no download. Next to Background it gives a sharper edge (hair included, as long as the screen is lit evenly) and uses a lot less of the computer. Switch it on under **Optional** in the Plugins tab of Settings.

- **Screen** is **Green**, **Blue** or **Picked**. **Picked** asks you to click the screen on the preview and keys whatever colour is there, which helps with a screen that isn't quite the usual green or with uneven light
- **Tolerance** sets how many shades of the screen go (raise it for creases and shadows, lower it if bits of you vanish). **Edges** softens the outline and **Spill** takes the green or blue glow off your edges and skin without touching colours far from the screen's
- **Behind you** is a **Colour** you pick or a **Picture** (PNG, JPEG, WebP or BMP) scaled to cover the frame
- Presets save it like the other picture settings

## Cinema tools

Optional, for judging focus and exposure while you set ISO, shutter and focus by hand. It also has a level for getting the horizon straight. Switch it on under **Optional** in the Plugins tab of Settings. Everything it draws stays on the preview and apps using the virtual camera never get any of it.

- **Focus peaking** outlines what's sharp in red (or green, blue or white). Turn the focus until the outline sits on the thing you care about
- **Zebras** stripe the parts of the picture that are about to clip to white. They start at 95% brightness and **Zebras from** under Video in Settings moves that between 70 and 100%
- **False colour** paints the picture by brightness: purple and blue for crushed and too dark, green for middle grey, pink for skin about a stop over it, yellow for nearly clipped and red for clipped. Everything well exposed in between turns grey. A small legend shows in the corner
- **Scope** puts a histogram, waveform, RGB parade or vectorscope in the bottom right of the preview. The waveform and parade show brightness from 0 at the bottom to 100% at the top, with a line every 25%. The vectorscope marks the colour bar targets and has a line for skin tones
- **Level** draws the horizon from the phone's motion sensors as a line between two fixed marks for level. Line it up with the marks and the guide turns green. The number next to it is how many degrees the picture is tilted and the one underneath is how far the camera looks up or down. It knows which camera is streaming and follows the flip and rotation under Picture. The phone only reads its motion sensors while the guide is on screen. Its app has to be up to date for it (an older one gets a note on the card instead)

Peaking colour and sensitivity and how big the scope is are under Video in Settings. The tools work on the preview's own smaller copy of the picture and the scopes are worked out on their own thread about 12 times a second, which keeps the stream itself as fast as before.

## Desk and document mode

Optional. Point the phone down at a sheet of paper, mark the sheet's four corners on the preview and Telescope straightens it so calls see it flat and head-on (good for homework help, a sketch or signing something). Switch it on under **Optional** in the Plugins tab of Settings.

- **Mark the sheet** puts four corners on the preview. Drag each one onto a corner of the sheet (or drag the middle to move all four) and press Enter or **Done**. Esc or **Cancel** leaves things as they were. While you're marking, the preview shows the camera's real picture. Corners left bunched up or in a line aren't used. Mark the sheet again with one on each corner
- The corners are tied to the camera's picture. Zooming, flipping or rotating afterwards leaves them on the sheet, but moving the phone or the paper doesn't and then **Mark the sheet again** starts from where they were
- **Writing** can make the text stand out. **Contrast** evens out the light across the paper so it turns white and the ink stays dark. **Black and white** turns everything into pure black ink on white paper, which works best for printed text and pen
- Clicking the straightened picture still works the way you'd expect: **Tap a point** focuses on the word you clicked, scrolling zooms around it and other plugins' picks land where you clicked. The lens outlines and the skeleton aren't drawn while the sheet is straightened
- If zooming or Auto framing pushes part of the sheet off the picture the card warns about it. Zoom out or mark it again
- The sheet keeps its own shape, centred with dark bars beside it (white ones with **Contrast** or **Black and white**), so a standing page stays a standing page in a wide call. **Shape** set to **Auto** works the real shape out from the corners, undoing the tilt. **A4**, **Letter** and **Square** use that size instead, turned the way the sheet lies. **Fill the picture** stretches it over the whole frame
- If the page still looks a bit squashed, **Width** and **Height** stretch it by eye from 50% to 200%. A double-click on a slider or **Reset** puts them back to 100%
- Each phone remembers its own corners and presets save them too

## QR codes

Optional. Switch on the QR codes card and hold a code up to the camera to get it on the clipboard. A Wi-Fi code gives you its password (or the network's name when it has none), handy for joining a café's Wi-Fi on the computer. A link or any other text is copied as it is. Once the code has read steadily for a moment it gets a green outline on the preview, and a notification says what was copied (**Notify me when a code is read** turns that off).

Telescope looks about three times a second at a smaller copy away from the stream itself and the picture doesn't slow down for it. The same code isn't copied again until it's been out of view for a few seconds. On Linux with Wayland a window in the background isn't allowed to change the clipboard. When that happens the card says so and **Copy** puts the code there with one click.

## Photos

Optional. Switch it on under **Optional** in the Plugins tab of Settings and the Photos card takes a real photo on the phone, at the camera's full quality instead of a frame of the video. It uses the lens, zoom, focus and manual settings the video has at that moment and turns the photo the same way as the preview. The card only shows what the current lens can do. On a lot of phones that's JPEG and a burst, while a flagship gets everything below:

- **Format** is JPEG with a quality setting, HEIC, Ultra HDR (Android 14) or RAW as a DNG, alone or together with a JPEG
- **Full resolution** under **Mode** uses the sensor's real size (the 50 or 200 MP on sensors that normally give 12). **Night**, **HDR** and **Bokeh** are the phone maker's own modes and show up if the maker lets other apps use them (Android 12)
- **Shots** can be a burst of up to 60, an exposure bracket for HDR merging or a focus bracket from near to far for focus stacking
- **Exposure** takes a long exposure up to what the sensor allows (30 s or more on some phones) at the video's ISO
- There's a 3 or 10 second **Timer**, best quality noise reduction and sharpening, and distortion correction can be turned on or off where the phone allows it

Photos go to the folder under **Save in** (a Telescope folder in your Pictures by default) with the phone's file name. **Open folder** shows the last ones. A plain photo is taken next to the video, which hitches for a moment the first time only. Night, HDR, Bokeh, long exposures and full resolution on some phones pause the video for a few seconds instead and the stream picks up again afterwards without a reconnect. **Take photo** also works as a [keyboard shortcut](#keyboard-shortcuts), even with Telescope in the background, and a photo taken that way pops up a notification once it's saved.

## Colour grading

An optional plugin for the look of the picture, switched on in Plugins (it needs no download). Like Background it runs on the computer and works with any phone and with Browser camera. Each phone gets its own grade and grades are saved in presets.

**Primaries** has four colour wheels. Lift is the shadows, Gamma the midtones, Gain the highlights and Offset all of it. Drag a wheel towards a colour to push the picture that way and use the slider under it for the level. Below them are Temperature and Tint, Contrast with its Pivot, Saturation and then Shadows and Highlights for brightening or darkening only the darker or brighter parts. A double-click puts any wheel, slider or curve back where it started. With the keyboard the arrows move a wheel's dot and Home centres it.

**Curves** has the RGB curves (all channels together, or red, green and blue on their own) plus Hue vs sat and Hue vs hue. On those two you pull a hue up to give it more colour or turn it towards the next hue along. Pulling it down does the opposite. Click to add a point and drag it, or double-click a point to take it off. In a curve that has keyboard focus **[** and **]** pick a point, the arrows move it and Delete removes it.

**LUT** puts a `.cube` file on top of the grade (1D or 3D, up to 65 points per side) with a Strength slider. **Export grade…** saves the whole grade, LUT included, as a 33 or 65 point `.cube` to get the same look in other software.

**Compare** splits the preview with the picture before the grade left of the line. Apps always get the graded picture. The switch in the card's header turns the grade off without losing it.

## Notes next to the lens

Optional, switched on under **Optional** in the Plugins tab of Settings. Type your notes on the Notes card and **Send to phone** puts them on the phone's screen in big text right under the front camera. Reading them there keeps your eyes almost on the lens and the people on the call see you looking at them. It needs the front camera and the phone facing you, and the phone app from the same release (with an older one the card asks you to update it).

- Each line you type is one step for **Next** and **Previous**. Both can go on a key in Keyboard shortcuts. A presenter clicker works too because it sends keys like a keyboard does
- **Auto** scrolls at the **Speed** you set once you press **Play**. **By hand** only moves with Next and Previous or when you drag the text on the phone
- **Text size** changes the size on the phone straight away. Edits to the text itself only show after you send again
- The card keeps a few sets of notes, named after their first line
- On the phone the screen stays on and black behind the text while the notes are up, which is easy on the battery with an OLED screen. Back closes them until you send again and stopping the stream takes them away

Android only lets an app open a screen while it's in front. With Telescope in the background the card asks you to open it on the phone and the notes appear once you do. With Preview open on the phone they wait until you close it, since opening them over it would make the stream stutter. Turned sideways in a holder, the text sits next to the lens instead of under it.

## The vision download

Auto framing, Background, Expose for my face and Privacy blur use MediaPipe to find people (the Fun pack can use it too). MediaPipe and its two models don't come with Telescope since most people won't use them. The first time you switch one of them on Telescope asks and downloads them from its GitHub releases (about 47 MB on Linux and 28 MB on Windows). They share the download and **Remove download** on the Optional card deletes it again.

It only works on 64-bit Intel and AMD computers for now.

## Name tag

Optional. It puts your name and pronouns in a corner of the picture, with a smaller second line under them if you want one (like your role). Switch it on under **Optional** in the Plugins tab of Settings and fill in the **Name tag** card.

- **Style** is **Bar** (a dark box with a coloured edge), **Pill**, **Light** or **Shadow**, which has no box at all and only a soft shadow behind the text
- **Position** can be any bottom corner, bottom centre or the top corners. **Size** is measured against the picture's height and the tag looks the same at 720p and 4K
- Presets save the tag too. One preset can have it on for a talk and another can leave it off for a chat
- Names in any script work (Arabic and Hebrew ones start from the right) and anything too long for the picture is cut short with "…"
- The tag goes on the picture before Telescope fits it to the virtual camera's canvas. With a **Canvas size** of another shape it sits in the corner of the picture rather than of the canvas. A low-resolution stream on a bigger canvas makes its text a little soft too (a higher resolution on the Stream output card helps)
- **Mirror** on the Transforms card never mirrors the text. If your own preview in a video call shows it backwards, that's the call app mirroring your self view while everyone else sees it the right way round

## Presets

The **Presets** button saves your camera, output and transform settings (lens included) under a name and brings them back in one click. The optional picture plugins (Auto framing, Background, Fun pack, Colour grading and the others) are saved too when they're on. Each phone has its own presets and each preset can have its own shortcut.

## Resolution and FPS

- The resolution list comes from what the current lens really supports
- One **FPS** setting (15 to 60) drives both the phone and the virtual camera. Rates the lens can't do are greyed out
- **Canvas size** under Video in Settings sets the virtual camera's size apart from the phone's. Pick 720p, 1080p or 4K in landscape or portrait, a 4:3 size or a custom one (custom sizes have to be even). On Linux changing it reloads the driver, which asks for your password once (close OBS first)

## Format and bandwidth

| Format | Good for | Needs |
|---|---|---|
| **Light** (H.264, the default) | Most things | About 8 Mbps at 1080p30, so fine even on slower Wi-Fi |
| **Heavy** (MJPEG) | Sharper fast motion | USB or strong Wi-Fi |

A phone that can't do Light switches to Heavy automatically.

- **Heavy** has a JPEG quality slider
- **Light** has a bitrate slider with **Auto**, a fixed 1-100 Mbps or **Dynamic**. Dynamic sends as much as the connection can carry and backs off before the video starts to lag. If that's still too much it lowers the frame rate too (down to 15) and climbs back once there's room
- **Hardware acceleration** under Video in Settings decodes Light on the graphics card instead of the processor, which matters most at 4K or on a laptop. It's off by default. It works on Windows with any graphics card and on Linux with NVIDIA. For AMD and Intel on Linux, **Set up VA-API…** next to it gets it working
- Light only lists resolutions the phone's encoder can take

<details>
<summary><b>UDP on Wi-Fi (experimental)</b></summary>

Under the connection settings. It sends Light over UDP instead of TCP while the phone is on Wi-Fi. On a busy network a lost packet then only costs a resend of that one piece instead of holding up the whole stream, which can cut lag spikes. It's encrypted and if the video doesn't get through (usually a firewall) it goes back to TCP. It isn't used over USB or for Heavy.

Dynamic works over UDP too. The computer tells the phone how many packets went missing and how long they took to arrive, which shows a thinning link earlier than waiting for video to pile up on the phone. If the phone app is older than the desktop app, Dynamic still runs over TCP.
</details>

## Microphone

The phone's mic can be a microphone on your computer while you stream.

- **Linux:** Telescope creates **Telescope Microphone** through PulseAudio or PipeWire. Most desktops need nothing extra
- **Windows:** needs [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (free). The card's **Install VB-Cable** downloads and opens VB-Audio's setup for you (Windows asks for admin and VB-Audio asks for a restart). Afterwards pick **CABLE Output** as the microphone in other apps and switch the mic off and on again in Telescope
- **Gain** goes from -24 to +12 dB (**Max gain** under Audio in Settings raises the top). The level meter turns yellow while the limiter is holding loud moments down and red when the sound clips. The limiter is on by default
- **Auto level** (off by default) evens out quiet and loud speech and stops adjusting while nobody talks
- **Noise reduction** (off by default) takes typing, fans and room hiss out of the mic with RNNoise running on your computer. It cleans up the same way for every phone and for a browser's mic, which helps most with phones whose own noise suppression is weak or missing. It adds about 20 ms of delay and switches in or out at the next pause in speech. With Discord, Zoom or a similar app turn off the app's own noise suppression while this is on (both at once can make voices sound thin). The level meter shows the mic as it arrives, before noise reduction
- **Mute** sends silence without disconnecting. It's in the card and the tray menu and resets when Telescope restarts
- **Mic only:** the camera button under the preview turns the phone's camera off while the mic stays on. Apps see the wait screen. Press it before starting and Start becomes **Start mic only**
- The tray icon shows what's live: a red dot for the camera and a mic for the mic (crossed out when muted)

## Browser camera

Stream from anything with a browser (an iPhone, a tablet, another laptop) with nothing to install on it. Pick **Browser camera** in the phone picker, scan the code and tap **Start** on the page. It uses WebRTC like browser video calls do, so any up-to-date browser works. Nothing goes through a server outside your network.

- **First visit warning:** the browser says the connection isn't private because Telescope makes its own certificate. It's still encrypted. On iPhone tap **Show Details** then **visit this website**. In Chrome it's **Advanced** then **Proceed**
- **Keep the page open with the screen on.** Phones pause the camera when you switch apps or lock and it picks up again when you come back
- **Black screen** turns the page black while the camera streams on to save battery on OLED phones. Double tap to get the page back. A single tap only shows the connection status for a few seconds. iPhones don't let web pages go full screen, so there it plays a black video in the iPhone's own player instead and closing the player brings the page back
- **Light** on the page turns on the flash where the browser allows it (Chrome on Android usually does). Front cameras get a white screen instead
- **Flip camera** shows on devices with more than one camera
- **New link** makes a fresh code and the old one stops working at once. Connected browsers are dropped and apps fall back to the wait screen
- Several devices can scan the same code. Each gets its own virtual camera (see [Several cameras](#several-cameras-at-once))
- The browser's microphone works with the Microphone card too. It's sent raw (no echo cancellation or noise suppression) because Discord, Zoom and similar do their own
- Browsers show up in the phone picker under their device name. One only gets saved settings once you change a setting. Saved browsers are listed under **Your phones** (the bin button forgets one) and are forgotten after 60 days without connecting
- Camera, output and alert settings don't apply to browsers. Resolution (480p to 1080p) and frame rate (15 to 30) are on the card
- The card shows what the browser really sends, like "H.264, 1280×720 at 30 fps". It can be lower than what you picked when the device or Wi-Fi can't keep up. If the device's processor is the limit the card warns and a lower resolution or frame rate usually helps
- On Linux the virtual camera is 1920x1080 unless you set another canvas under Video in Settings

## Several cameras at once

Click the **+** next to Stop while streaming to add another phone (or Browser camera), up to four in total. Each gets its own virtual camera: **Phone Camera 2** to **4** on Linux and **Telescope #2** to **#4** on Windows.

- The first time Telescope adds those cameras and asks for your password (Linux) or permission (Windows). **Extra cameras** under Virtual camera & phone in Settings removes them again
- A row of tiles shows every stream, which camera it goes to and whether it's live. Click a tile to bring its settings into the panels. The **x** on a tile stops only that stream and Stop stops them all
- Picture effects belong to the camera you set them on. Turning on a Fun pack look, Background, Chroma key, Auto framing or Expose for my face for one stream leaves the others as they were, just like Transforms, Desk mode and Colour grading always did
- A phone gets the same virtual camera slot as last time if it's free, to lower the need for redoing OBS scenes
- Only one stream has the mic at a time (the first to start). To move it, click another tile and turn on **Phone mic**
- With more than one stream mic only isn't available, the canvas can't be resized and Automatic streaming won't stop anything until all of them have stopped
- Extra cameras are always the size set in **Canvas size** under Video in Settings (1920x1080 on Auto). Other shapes get black bars

## Several cameras in one picture

Optional (switch it on under **Optional** in the Plugins tab of Settings). It puts the streams you run into one picture for call apps that take only one camera, like your face big with the desk small in a corner, two phones side by side or a grid of up to four.

- The switch on the **One picture** card turns it on and **Edit picture…** opens the editor with a live preview
- Drag a stream to move it. Arrow keys nudge it a pixel at a time
- The handles on the corners and sides resize it the way OBS does. A plain drag scales it without changing its shape. Hold Shift to stretch or squash it into any shape and Alt to crop the side you're dragging instead (each side remembers its own crop)
- Edges snap to the picture's edges and middle and to the other streams. Hold Ctrl to place one freely
- Scroll over a stream to zoom into it and drag it with the right mouse button to pick which part shows
- **Fit its shape** puts a stream back to its own shape with nothing cropped, stretched or zoomed
- Each stream can get a border and rounded corners. Behind them goes a colour or a picture
- **Start from** lays out the running streams **Side by side**, **Big and small** or in a **Grid** to tweak from there
- Layouts can be saved under a name and picked on the card. A keyboard shortcut can switch to one and a preset brings back the picture it was saved with (without touching your saved layouts)
- The picture goes out on the **Telescope** camera, the one call apps use, whichever stream goes there (usually the first you started). The other streams still go to their own cameras as well, for OBS scenes that use them one by one. While no stream goes to Telescope, or the one there has its camera off, the picture waits
- It only takes over while at least two of its streams are running. A stream that drops leaves the picture after 3 seconds and is back in it once it reconnects. The others don't freeze with it, even when it's the one on Telescope
- The picture runs at the frame rate of the stream going to Telescope. Its size is the canvas size under Video in Settings, or on Auto the size the Telescope camera opened at

## On Air sign

Optional. It lets the people you live with know you're on a call without you having to tell them. Switch it on under **Optional** in the Plugins tab of Settings and it gets an **On Air** tab of its own.

- **On air when** decides what counts. By default it's an app using the virtual camera, even while it only sees the wait screen. It can also count Telescope's mic being live (streaming and not muted), the mic alone or any stream at all
- It waits a second before going on air and six before going off again, because apps like to close and reopen the camera when you change their settings
- **Sign on another device** shows a big On air or Off air page in any browser on your network. Scan the code with an old tablet or phone and tap the page to make it full screen. The screen stays on where the browser allows that. The link stays the same after Telescope restarts until you click **New link** and the page only answers to that link. The browser warns about the certificate the first time, the same as with [Browser camera](#browser-camera)
- **Webhook** sends a request every time you go on or off air, for Home Assistant or a smart plug or anything else that takes one. POST sends what's in **Going on air** or **Going off air** as the body (`{"on_air": true}` when left empty) and GET adds it to the address instead. **Test on** and **Test off** send one right away
- If the webhook stops answering you get one quiet banner about it and no more of them until it has worked again. Meanwhile Telescope tries the latest state again after 5 seconds, 15 seconds and a minute, then every 5 minutes, which puts the light right after a Wi-Fi blip or a hub restart. It also sends the current state when it starts. Quitting while on air sends the off request first
- While you're on air an **On air** button shows next to the settings button and takes you to the tab

## Monitoring

- FPS and throughput sit in the footer. When the stream can't keep up a note suggests what to try, with a button for it
- A dropped stream reconnects over whichever route works (pull the cable and it carries on over Wi-Fi). After 10 seconds apps get the wait screen instead of a frozen frame and after 30 a banner says the phone can't be reached
- Phone battery and temperature have alerts that can notify you, stop the stream or both. A stop explains why and offers **Start anyway**. A phone that runs hot also lowers its own frame rate first (Android 10 and newer)
- **Open log**, **Copy diagnostics** and **Report a bug** are under Updates & About in Settings. Report a bug copies the diagnostics and opens the bug form on GitHub for you to paste them in. Addresses, tokens and your home folder are stripped out

## Phones, pairing and connection

- **Add phone** pairs by QR code or by plugging the phone in over USB (USB needs adb, which Windows downloads from Google after asking)
- The **?** next to it brings the setup checklist back. Click it again to close it
- You can pair several phones to one computer and one phone to several computers
- Telescope uses USB when the phone is plugged in and answering and Wi-Fi otherwise. If a cable is plugged in but isn't used the Connection panel says why. **Connect via** can force either one
- A new IP address from the router breaks nothing, since the phone announces itself on the network
- On a slow Wi-Fi stream the note tells you when a weak signal or 2.4 GHz is the cause, since moving closer, a 5 GHz network or a cable fixes that better than lowering quality
- The phone's screen stays on while the app is open because Android only runs Wi-Fi at full speed with the screen on. You can press the power button and the stream keeps going
- Camera, output, transform and alert settings are saved per phone and so are picture effects like Background or the Fun pack. Connection settings and the canvas are shared

## Privacy

- Everything between the phone and computer is encrypted with TLS. The desktop remembers your phone's certificate at pairing and refuses anything that shows up with a different one
- The pairing token only travels in the QR code (or over adb) and inside the encrypted connection
- Someone on the network can still see that a stream is running and roughly how much data it moves. **Local only - USB** in the Android app takes the stream and controls off the network entirely
- **Wait for my computer** (off by default) lets you start the camera from your PC with the phone's screen off or the app closed. Tap **Stop waiting** in its notification to end it
- Browser camera only listens on port 8767 while it is picked or a browser is streaming. Anyone can load the page but sending video needs the token in the code, which changes on every start and on **New link**. Its certificate isn't pinned like a phone's, so on a network you don't trust use a phone
- The On Air sign listens on port 8768 while it's switched on. It serves nothing but the sign page and whether you're on air (that part only to its link's token)
- The apps only go online to check GitHub for updates. On Windows the desktop also fetches adb from Google and VB-Cable from VB-Audio, but only after you click to get them
- On Linux only your user can read the config folder with the pairing tokens. On Windows the camera driver sits in Program Files where other programs can't swap it

## Updates

- Both apps check at launch and once a day, on **Stable** or **Nightly**
- **Desktop:** an **Update** button appears in the header and installs in one click (not while streaming). If an update gets cut short or the new version won't start the next launch finishes it or puts the old one back
- **Phone:** the update card downloads, checks and installs the new APK. If the phone app is older than the desktop the Connection panel says so and can update it over USB

<details>
<summary><b>Updating an unzipped Windows copy</b></summary>

Updating installs the new version for your user instead (Start menu entry and uninstaller, no admin) and then deletes the unzipped folder. Only Telescope's own files go. Settings, paired phones and adb come along and a desktop shortcut gets swapped for one to the installed copy. Copies on a USB stick or network drive update in place. Anywhere else you can tick **Keep this copy where it is** in Updates to do the same.
</details>

## System integration

- **Automatic streaming** (Streaming in Settings, off by default) starts when the phone is ready or when an app opens the camera. It stops once no app has used the camera for **Wait before stopping** (15 seconds by default). With the phone mic on it only turns the camera off. After you press Stop it won't start again until the phone or the app goes away and comes back
- **Wait screen** is what apps see while the phone isn't streaming. Use the default or your own image or GIF. **Mirror** is for apps that flip the camera
- **Open Telescope when I sign in** (General in Settings) starts it quietly in the tray
- Closing the window while streaming hides Telescope to the tray
- When Start can't go ahead a banner says why and has a button that fixes it
- While the window is in the tray Telescope checks on the phone less often to save its battery

<details>
<summary><b>Folding cards</b></summary>

Click a card's title to fold it down to its header and click again to open it. Section titles inside a card work the same way. Folds are remembered after a restart and saved for the whole app, not per phone. Shortcuts keep working on folded controls. Tab reaches the titles and Space or Enter folds them.

**Monochrome** under **Card colours** in General puts every card in the app's purple instead of its own colour.
</details>

## Keyboard shortcuts

- The **Keyboard shortcuts** tab in Settings binds a key to almost any control. You can also right-click a control in the main window to add one for it
- Each shortcut has its own settings. One key can raise exposure by 1 EV while another lowers it by 0.3 EV. Switches can toggle or work only while the key is held, which makes **Mute** a push to talk
- **Settings search** (Ctrl+F) finds settings by what they do ("gpu" leads to Hardware acceleration)

<details>
<summary><b>Global shortcuts and the command line</b></summary>

Shortcuts are global by default and also work while another app is in front.

- **Windows:** Telescope claims the key from Windows. If another app already has it Windows gives it to that app even while Telescope is in front. Pick another key
- **Linux:** it goes through the desktop's global shortcuts portal (Plasma 5.27 and later, GNOME 48 and later, Hyprland). The first time the desktop asks you to confirm the keys and may pick different ones. **Open** next to **Desktop shortcut settings** opens the place to change them
- **Other desktops:** shortcuts only work inside Telescope. To make them global bind a command in the desktop's own keyboard settings. Right-click a shortcut and pick **Copy command line**, or use `telescope --action ID name=value` (for example `--action microphone.mute mode=toggle`). `--list-actions` lists every action

**Notify when one runs** shows a quick confirmation while Telescope is in the background. It's off by default.
</details>

## Plugins

Most of the desktop app is plugins, and the **Plugins** tab in Settings switches them on and off. Changes apply after a restart (the plugin's row gets a **Restart now** button). Switched off plugins keep their settings and a banner names what's off after the restart. You can also install plugins other people made. See [Plugins](plugins.md).
