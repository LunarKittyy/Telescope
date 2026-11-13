# Sources and layouts

Other cameras and the browser, and putting several cameras on screen.

## Browser camera

Stream from anything with a browser (an iPhone, a tablet, another laptop) with nothing to install on it. Pick **Browser camera** in the device picker, scan the code and tap **Start** on the page. It uses WebRTC like browser video calls do, so any up-to-date browser works. Nothing goes through a server outside your network.

- **First visit warning:** the browser says the connection isn't private because Telescope makes its own certificate. It's still encrypted. On iPhone tap **Show Details** then **visit this website**. In Chrome it's **Advanced** then **Proceed**
- **Keep the page open with the screen on.** Phones pause the camera when you switch apps or lock and it picks up again when you come back
- **Black screen** turns the page black while the camera streams on to save battery on OLED phones. Double tap to get the page back. A single tap only shows the connection status for a few seconds. iPhones don't let web pages go full screen, so there it plays a black video in the iPhone's own player instead and closing the player brings the page back
- **Light** on the page turns on the flash where the browser allows it (Chrome on Android usually does). Front cameras get a white screen instead
- **Flip camera** shows on devices with more than one camera
- **New link** makes a fresh code and the old one stops working at once. Connected browsers are dropped and apps fall back to the wait screen
- Several devices can scan the same code. Each gets its own virtual camera (see [Several cameras](#several-cameras-at-once))
- The browser's microphone works with the Microphone card too. It's sent raw (no echo cancellation or noise suppression) because Discord, Zoom and similar do their own
- Browsers show up in the device picker under their device name. One only gets saved settings once you change a setting. Saved browsers are listed under **Your devices** (the bin button forgets one) and are forgotten after 60 days without connecting
- Camera, output and alert settings don't apply to browsers. Resolution (480p to 1080p) and frame rate (15 to 30) are on the card
- The card shows what the browser really sends, like "H.264, 1280×720 at 30 fps". It can be lower than what you picked when the device or Wi-Fi can't keep up. If the device's processor is the limit the card warns and a lower resolution or frame rate usually helps
- On Linux the virtual camera is 1920x1080 unless you set another canvas under Video in Settings

## Webcams and capture cards

A webcam plugged into the computer streams just like a phone. It shows up in the device picker under its own name and every effect works on it (Background, Chroma key, Colour grading, Auto framing, Transforms, One picture and the rest). The result goes out on Telescope's virtual camera like a phone's would. With **+** it can stream next to a phone or another webcam (see [Several cameras](#several-cameras-at-once)).

- **Cameras with a webcam mode** show up here too. Many mirrorless cameras and some DSLRs have a USB webcam (UVC) mode. Any camera with a clean HDMI output works through an HDMI capture card too, since computers see the card as a webcam. Cameras that only do live view over USB (many older DSLRs) work through gphoto2, see below. Maker software isn't supported
- **Older DSLRs over USB (Linux only):** plug the camera in and, if it can do live view, it shows in the picker next to the webcams. The Linux download carries what it needs (libgphoto2), so there is nothing to install. Expect a small picture (a Nikon D5100 sends 640×426 at about 30 fps). The Webcam card shows what the camera lets you change right now: a focus jog with a Focus button beside it (drag the jog toward Near or Far and the focus keeps moving, in bigger steps the further out you hold it, and it stops when you let go), Aperture and Exposure compensation as dials you step with the arrows or the mouse wheel, and White balance. On a Nikon that means the dial has to be on M, since the other modes refuse most changes and the card then shows none. A change the camera makes itself, like zooming the lens (which changes the aperture and the apertures it offers) or turning the mode dial, shows up on the card within a couple of seconds. ISO and shutter speed aren't there because live view doesn't show them. On a Nikon D5100 a new aperture only reaches the picture once live view starts again, so about a second after you stop changing it the picture freezes briefly and the mirror clicks (several steps in a row cost one restart). Every few minutes the camera ends live view and Telescope starts it again, which clicks the mirror and drops about a second of frames while the stream carries on. If the stream says the camera is mounted as a drive, unmount it in your file manager and start it again. Not every camera can do this (some only show up as a USB drive). Your user needs access to the camera's USB device, which the udev rules of most distros give you. Windows has no libgphoto2, so there it needs a webcam mode or a capture card
- **Camera quirks (for contributors):** cameras that behave differently are listed in one table, `rust/crates/outputs/src/webcam_capture/quirks.rs`, and the rest of the code asks it by kind of quirk, never by model. To add a camera, put its model name as libgphoto2 lists it (`gphoto2 --auto-detect`) and the quirk in the table with a note of where you saw it, but only once you've confirmed it on the real camera. A new kind of quirk is a new variant with a test against the fake camera.
- **Plugging in and out** is noticed while Telescope runs. A webcam that's unplugged while it streams keeps its place, apps see the wait screen and the stream carries on once it's plugged back in. One that isn't streaming just leaves the picker
- **Resolution and Frame rate** are on the Webcam card, kept for each camera. **Auto** takes the largest size up to 1080p the camera can send at the frame rate picked. The card shows what the camera really sends, like "1920×1080, MJPEG, 30 fps"
- **Picture settings** the webcam itself offers are on the Webcam card too: exposure (time shown as a shutter speed like 1/60), gain, white balance, focus, zoom, anti-flicker and the like. Only what the camera has shows up, with its own range, and a manual slider stays greyed out while its Auto is on. Each camera keeps its own and gets them back every time it's opened, since most webcams forget them when unplugged. **Reset** puts the camera's defaults back. Anti-flicker starts on your mains frequency (50 or 60 Hz, guessed from the time zone) so lights don't flicker or band. On Windows some cameras don't offer anti-flicker or the frame-rate switch, and those rows are left out
- **MJPEG or raw:** Telescope asks for raw video when the camera can send it at that size and rate and for MJPEG when it can't. A USB 2 webcam usually can't fit raw 1080p at 30 fps through the cable, which is why the bigger sizes tend to come as MJPEG
- **In use somewhere else:** a webcam can only stream to one app at a time on Linux (Windows sometimes shares it). With another app holding it a banner names that app (where Telescope can tell) and the stream starts as soon as the app lets go
- Telescope's own virtual cameras are never listed, which keeps a stream from looping back into itself. Other virtual cameras (OBS Virtual Camera and similar) are left out too
- A webcam's microphone isn't passed through the Microphone card. Pick it in your call app instead
- The phone's Camera card, Stream output and the phone's alerts don't apply to webcams. Switching off **Webcams** in Plugins takes them out of the picker

## Several cameras at once

Click the **+** next to Stop while streaming to add another phone (or Browser camera, or a webcam), up to four in total. Each gets its own virtual camera: **Phone Camera 2** to **4** on Linux and **Telescope #2** to **#4** on Windows.

- The first time Telescope adds those cameras and asks for your password (Linux) or permission (Windows). **Extra cameras** under Virtual camera & phone in Settings removes them again
- A row of tiles shows every stream, which camera it goes to and whether it's live. Click a tile to bring its settings into the panels. The **x** on a tile stops only that stream and Stop stops them all
- Picture effects belong to the camera you set them on. Turning on a Fun pack look, Background, Chroma key, Auto framing, Expose for my face or a Name tag for one stream leaves the others as they were, just like Transforms, Desk mode and Colour grading always did
- A phone gets the same virtual camera slot as last time if it's free, to lower the need for redoing OBS scenes
- Only one stream has the mic at a time (the first to start). To move it, click another tile and turn on **Phone mic**
- With more than one stream mic only isn't available, the canvas can't be resized and Automatic streaming won't stop anything until all of them have stopped
- Extra cameras are always the size set in **Canvas size** under Video in Settings (1920x1080 on Auto). Other shapes get black bars

## Several cameras in one picture

Optional (switch it on under **Streaming and devices** in the Plugins tab of Settings). It puts the streams you run into one picture for call apps that take only one camera, like your face big with the desk small in a corner, two phones side by side or a grid of up to four.

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
