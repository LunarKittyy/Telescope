<!-- covers: rust/crates/effects/src/stargazer* rust/crates/effects/src/phototrap* rust/crates/effects/src/photos* rust/crates/effects/src/desk* rust/crates/effects/src/notes* rust/crates/effects/src/cinema* rust/crates/effects/src/qr_reader* rust/crates/effects/src/stereo_plugin* rust/crates/system/src/mcp*.rs rust/crates/shell/src/plugins/presets.rs; checked: 88de924b5 -->
# Things to try with Telescope

This page is for the ideas that don't come to mind first, from a phone on the other side of the world to a camera pointed at the night sky. Each one is a few steps and some are for fun. The optional plugins are switched on under **Plugins** in Settings and need a restart, see [Switching plugins on and off](features/plugins.md#switching-plugins-on-and-off).

## Use a phone from far away

Telescope doesn't care where the phone is as long as the two can reach each other. [Tailscale](https://tailscale.com) is the easy way to get there. It gives every device an address that works from anywhere and Telescope already knows how to use it.

1. Install Tailscale on the phone and the computer and sign in to the same tailnet.
2. Open Telescope on both and press **Add device**.
3. Scan the code. The stream, the effects and the virtual camera work the same.

Some things to do with that:

- **A friend as a remote guest.** Their phone shows up in your OBS as a direct camera feed that looks much sharper than a screen-captured video call. Tailscale can share a single machine with them so they don't get the rest of your devices. The code works like a password. Only show it to the person who is meant to scan it.
- **A phone left at the cottage.** Plug it into power and turn on **Wait for my computer** in the phone app. You can then start the camera from your desk without touching the phone.
- **Showing someone something.** Pair your phone with their Telescope and use your camera.

Ports, access rules and checking the connection are in [Over Tailscale](features/connection.md#over-tailscale).

## Take photos with a DSLR from your computer

Use Telescope as a big live preview and an easier settings console for a DSLR while you take photos. This works with Nikon and Canon cameras on Linux and Windows.

1. Plug the camera in with live view on. A Nikon needs its dial on M. It shows up in the device picker next to the webcams.
2. Switch on **Photos** under **Camera** and restart. Start the stream.
3. The **Webcam** card has what the camera allows you to change right now, like white balance, focus and aperture. The **Photos** card has **ISO**, **Shutter** and **How many**.
4. Press **Take photo**. RAW and JPEG together work if the camera supports them. The photos go to the **Save in** folder.

Older cameras send a small picture. More in [Webcams and capture cards](features/sources.md#webcams-and-capture-cards) and [Photos](features/effects.md#photos).

## Use it as a field monitor for filming

Cinema tools put the checks of a field monitor on the big preview while you set ISO, shutter and focus manually. None of it goes out to the apps using the virtual camera.

1. Switch on **Cinema tools** under **Preview tools** and restart.
2. Start the stream and turn on **Focus peaking** to outline what's sharp. Turn the focus until the outline sits on the subject.
3. **Zebras** stripe what's about to clip to white and **False colour** paints the picture by brightness. **Scope** adds a histogram, waveform, RGB parade or vectorscope.

**Guides** and **Level** help with framing and the horizon. More in [Cinema tools](features/effects.md#cinema-tools).

## Look at the night sky

Clip the phone to a telescope's eyepiece (an actual one, not the name of the program :)) and Stargazer stacks the frames into a sharper moon or planet, a clearer faint sky or star trails. It's an early version.

1. Switch on **Stargazer** under **Camera** and restart.
2. Start the stream with the phone on the eyepiece. Turn the focuser until the **Focus** number on the card is as high as it goes. **Low light** on the Camera card makes each frame catch more light in the dark.
3. Pick **Lucky** (lucky imaging) for the moon and planets, **Live** for faint things or **Trails** for star trails, then press **Start**. For a faint sky, set **Frames from** to **Photos** to stack long photos instead of video frames.
4. The finished stack is saved as a PNG in Pictures/Telescope/Stargazer. **Show the stack on the camera** also sends it to the virtual camera.

More in [Stargazer](features/effects.md#stargazer).

## Watch animals while you're away

Point the phone at a bird feeder or a garden path and let Phototrap take a burst of photos whenever something moves. No photo is saved until something moves.

1. Switch on **Phototrap** under **Camera** and restart.
2. Start the stream with the phone pointing where the animals go.
3. Press **Draw zone** on the card and mark the area on the preview. You can draw up to four. With none, the entire picture is watched.
4. Set how many **Photos** a burst takes, the **Pause** before it can go off again and the **Sensitivity**. A higher one catches smaller movements and more false alarms.
5. Turn on **Armed**. The photos go to the **Save in** folder.

A phone left outside needs power and a dry spot. **Wait for my computer** in the phone app makes it possible to start it from your desk. For a phone far away see [Use a phone from far away](#use-a-phone-from-far-away).

## Put a phone on a microscope

Hold the phone over the eyepiece and the computer's window is a much bigger screen than the phone's.

1. Start the stream with the camera on the eyepiece.
2. Set **Focus** to **Manual** on the Camera card and turn it until the picture is sharp.
3. Set **ISO and shutter speed** yourself if the light changes a lot. **Brightness** is there too.

More in [Camera control](features/camera.md#camera-control).

## Switch between you and a whiteboard

Save two scenes as presets. One is you talking and the other is the whiteboard behind you, cut out and straightened. Recalling a preset switches between them.

1. Switch on **Desk and document mode** under **Camera** and restart.
2. Start the stream and set up the picture for talking. Click **Presets** and save it, as "Talking" for example.
3. Turn on **Desk mode** and press **Mark the sheet**. Drag the four corners onto the corners of the board and press **Done**. It can be any flat sheet or board and doesn't have to be paper. **Contrast** evens out the light across it.
4. Save that as a second preset, "Board". Click a preset to switch.
5. Under **Keyboard shortcuts** in Settings add a shortcut for **Apply a preset** once per scene and pick the preset. Each scene then has its own key.

With two cameras, one on you and one on the board, set Desk mode on the board's camera and combine them in **One picture**. Layouts have shortcuts too. See [Presets](features/camera.md#presets), [Desk and document mode](features/picture.md#desk-and-document-mode) and [Several cameras in one picture](features/sources.md#several-cameras-in-one-picture).

## Read a script while looking at the lens

The teleprompter puts your text on the phone's screen right under the front camera. You look almost at the lens while you read, which works for recording a message or practising a talk.

1. Switch on **Teleprompter** under **On screen** and restart.
2. Use the front camera with the phone facing you. Type your lines on the **Teleprompter** card and press **Send to phone**.
3. Each line is a step for **Next** and **Previous**, which can go on keys in Keyboard shortcuts. A presenter clicker works too since it sends keys. **Auto** scrolls at the **Speed** you set once you press **Play**.

More in [Notes next to the lens](features/effects.md#notes-next-to-the-lens).

## Make 3D with two phones

Two phones streaming at the same time become one 3D picture. It works as **Red/cyan** for glasses or **Side by side** for a headset or a 3D screen. It's an early version. Two phones of the same model work best.

1. Switch on **3D** under **Streaming and devices** and restart.
2. Start one phone, then click **+** next to Stop and add the second.
3. Turn on the switch on the **3D** card and pick the **Look**. **Swap eyes** fixes a picture that looks inside out.

**Fine tune** and **Match** line up and even out the two pictures. More in [3D from two phones](features/sources.md#3d-from-two-phones).

## Use a friend's iPhone as a second camera

Browser camera needs no install on the other device. An iPhone, a tablet or another laptop works.

1. Pick **Browser camera** in the device picker, or click **+** to add it next to a phone.
2. Scan the code with the other device and tap **Start** on the page. On the first visit the browser warns that the connection isn't private. Telescope makes its own certificate. Go on anyway.
3. Leave the page open with the screen on.

Each device that scans the code gets its own virtual camera. More in [Browser camera](features/sources.md#browser-camera).

## Join the café Wi-Fi with its QR code

Hold the code up to the camera and Telescope copies the password to the clipboard.

1. Switch on **QR code reader** under **Preview tools** and restart.
2. Start the stream and hold the Wi-Fi code in front of the camera. A green outline marks it and a notification shows what was copied.
3. Paste the password in the Wi-Fi dialog on the computer.

A link or any other text is copied unchanged. More in [QR codes](features/effects.md#qr-codes).

## Ask an AI app to tune your look

With the MCP server an AI app can look at Telescope and change its settings. Describe the look you want and it can set the colour grading.

1. Switch on **MCP server** under **Extras** and restart.
2. Add Telescope to your AI app's MCP settings, see [MCP server](features/mcp.md#adding-it-to-an-ai-app).
3. Describe what you want. The right half of the **MCP** button in the top bar undoes what the AI changed.

Set **Access** to **Read only** on the MCP tab if you only want it to look. More in [MCP server](features/mcp.md).
