<!-- covers: rust/crates/link/src/net.rs rust/crates/link/src/pairing.rs rust/crates/link/src/phones.rs android/app/src/main/kotlin/com/telescope/Pairing.kt rust/crates/effects/src/stargazer* rust/crates/effects/src/phototrap* rust/crates/effects/src/photos* rust/crates/effects/src/desk* rust/crates/effects/src/notes* rust/crates/effects/src/cinema* rust/crates/effects/src/qr_reader* rust/crates/effects/src/stereo_plugin* rust/crates/system/src/mcp*.rs rust/crates/shell/src/plugins/presets.rs; checked: d369c8664 -->
# Things to try with Telescope

Here are a few ideas for Telescope that you might not think of first, from a phone on the other side of the world to a camera pointed at the night sky. Each one is a few steps. The optional plugins are switched on under **Plugins** in Settings and need a restart, see [Switching plugins on and off](features/plugins.md#switching-plugins-on-and-off).

## Use a phone from far away

Telescope doesn't care where the phone is as long as the two can reach each other. [Tailscale](https://tailscale.com) is the easy way to get there, since it gives every device an address that works from anywhere and Telescope already knows how to use it.

1. Install Tailscale on the phone and the computer and sign in to the same tailnet.
2. Open Telescope on both and press **Add phone**.
3. Scan the code. The stream, the effects and the virtual camera work the same.

It works for a friend's phone as a guest camera in your OBS, a phone left at the cottage or your own phone showing someone something at their place. The details are [further down](#tailscale-in-more-detail).

## Take photos with a DSLR from your computer

Use Telescope as a big live preview and an easier settings console for a DSLR while you take photos. This works on Linux. On Windows a Nikon or Canon shows live view but taking photos isn't there yet.

1. Plug the camera in with live view on. A Nikon needs the dial on M because the other modes refuse most changes. It shows up in the device picker next to the webcams.
2. Switch on **Photos** under **Camera** and restart. Start the stream.
3. The **Webcam** card has what the camera lets you change right now, like white balance, focus and aperture. The **Photos** card has **ISO**, **Shutter** and **How many**.
4. Press **Take photo**. RAW and JPEG together work if the camera supports them. The photos go to the **Save in** folder.

The preview is small, since a Nikon D5100 sends 640×426. More in [Webcams and capture cards](features/sources.md#webcams-and-capture-cards) and [Photos](features/effects.md#photos).

## Use it as a field monitor for filming

Cinema tools put the checks of a field monitor on the big preview while you set ISO, shutter and focus by hand. None of it goes out to the apps using the virtual camera.

1. Switch on **Cinema tools** under **Preview tools** and restart.
2. Start the stream and turn on **Focus peaking** to outline what's sharp. Turn the focus until the outline sits on what matters.
3. **Zebras** stripe what's about to clip to white and **False colour** paints the picture by brightness. **Scope** adds a histogram, waveform, RGB parade or vectorscope.

**Guides** and **Level** help with framing and the horizon. More in [Cinema tools](features/effects.md#cinema-tools).

## Look at the night sky

Clip the phone to a telescope's eyepiece and Stargazer stacks the frames into a sharper moon or planet, a clearer faint sky or star trails. It's an early version and tagged Lab.

1. Switch on **Stargazer** under **Camera** and restart.
2. Start the stream with the phone on the eyepiece. Turn the focuser until the **Focus** number on the card is as high as it goes. **Low light** on the Camera card lets each frame catch more light in the dark.
3. Pick **Lucky imaging** for the moon and planets, **Live stack** for faint things or **Star trails**, then press **Start**.
4. The finished stack is saved as a PNG in Pictures/Telescope/Stargazer. **Show the stack on the camera** also sends it to the virtual camera.

More in [Stargazer](features/effects.md#stargazer).

## Watch animals while you're away

Point the phone at a bird feeder or a garden path and let Phototrap take a burst of photos whenever something moves. Nothing is saved until it does.

1. Switch on **Phototrap** under **Camera** and restart.
2. Start the stream with the phone pointing where the animals go.
3. Press **Draw zone** on the card and mark the area on the preview. You can draw up to four. With none, the whole picture is watched.
4. Set how many **Photos** a burst takes, the **Pause** before it can go off again and the **Sensitivity**. A higher one catches smaller movements and more false alarms.
5. Turn on **Armed**. The photos go to the **Save in** folder.

A phone left outside needs power and a dry spot. **Wait for my computer** in the phone app lets you start it from your desk, see [A phone you leave somewhere](#a-phone-you-leave-somewhere).

## Put a phone on a microscope

Hold the phone over the eyepiece and the computer's window is a much bigger screen than the phone's.

1. Start the stream with the camera on the eyepiece.
2. Set **Focus** to **Manual** on the Camera card and turn it until the picture is sharp.
3. Set **ISO and shutter speed** by hand if the light changes a lot. **Brightness** is there too.

The controls on offer follow what the phone's lens can do, see [Camera control](features/camera.md#camera-control).

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

Two phones streaming at the same time become one 3D picture. It works as **Red/cyan** for glasses or **Side by side** for a headset or a 3D screen. It's an early version and tagged Lab. Two phones of the same model work best.

1. Switch on **3D** under **Streaming and devices** and restart.
2. Start one phone, then click **+** next to Stop and add the second.
3. Turn on the switch on the **3D** card and pick the **Look**. **Swap eyes** fixes a picture that looks inside out.

**Fine tune** and **Match** line up and even out the two pictures. More in [3D from two phones](features/sources.md#3d-from-two-phones).

## Use a friend's iPhone as a second camera

Browser camera needs nothing installed on the other device. An iPhone, a tablet or another laptop works.

1. Pick **Browser camera** in the device picker, or click **+** to add it next to a phone.
2. Scan the code with the other device and tap **Start** on the page. The browser warns that the connection isn't private on the first visit. On iPhone tap **Show Details**, then **visit this website**.
3. Leave the page open with the screen on.

Each device that scans the code gets its own virtual camera. More in [Browser camera](features/sources.md#browser-camera).

## Join the café Wi-Fi with its QR code

Hold the code up to the camera and Telescope copies the password to the clipboard.

1. Switch on **QR code reader** under **Preview tools** and restart.
2. Start the stream and hold the Wi-Fi code in front of the camera. A green outline marks it and a notification shows what was copied.
3. Paste the password in the Wi-Fi dialog on the computer.

A link or any other text is copied as it is. More in [QR codes](features/effects.md#qr-codes).

## Ask an AI app to tune your look

The MCP server lets an AI app look at Telescope and change its settings. Describe the look you want and it can set the colour grading.

1. Switch on **MCP server** under **Extras** and restart.
2. Add Telescope to your AI app's MCP settings, see [MCP server](features/mcp.md#adding-it-to-an-ai-app).
3. Describe what you want. The right half of the **MCP** button in the top bar undoes what the AI changed.

**Access** on the MCP tab can be set to **Read only**. More in [MCP server](features/mcp.md).

## Tailscale in more detail

[Tailscale](https://tailscale.com) puts your devices on one private network (a tailnet) with an address like `100.x.y.z` each, wherever they are. Nothing needs switching on in Telescope.

How it works with Telescope:

- The QR code lists the computer's addresses, LAN ones first and the Tailscale one after. The phone tries the LAN ones over Wi-Fi first, then every address on whatever network it has. Away from home that ends on the Tailscale address. Each attempt gets 2 seconds and the whole run 12.
- The computer remembers all of the phone's addresses. Later it tries the phone's address it found on the local network, then the last one that worked, then the rest with Tailscale first.
- Pairing needs the **Add phone** dialog open on the computer, because that's the only time its pairing server on port 8765 runs. The phone has to reach the computer for this. After that the computer reaches out to the phone.
- If you use Tailscale access rules, the phone needs to reach the computer on 8765 while pairing. The computer needs to reach the phone on 8766 and 8080. [Browser camera](features/sources.md#browser-camera) uses 8767 on the computer. Tailscale's default rules allow all of this.

### Someone else's phone as a camera

A friend joins your tailnet (Tailscale can share a single machine with them so they don't get the rest of your devices), pairs their phone with your Telescope and shows up in OBS as a remote guest. It's a direct camera feed instead of a screen-captured video call and looks a lot sharper.

The QR code carries the token that lets a phone pair and works like a password. Only show it to the person who should scan it. Close **Add phone** once they have. One code pairs one phone. Remove the guest's phone on the computer when they are done and Telescope tells the phone to forget the computer.

### A phone you leave somewhere

A phone at the cottage or a stargazer rig in the garden that you watch from your desk. Pair it once while you're there or over the tailnet if you can reach it. After that it works from anywhere. **Wait for my computer** in the phone app lets you start the camera from your computer without touching the phone, which matters when it's out of reach.

Plug it into power. Some phones still close apps in the background. Check that Telescope survives a night on yours before relying on it.

### Helping someone remotely

You can pair your phone with their Telescope the same way and use your camera to show them something. They open **Add phone**, you scan the code (the code they show you works like a password) and you remove your phone from their list when you're done.

### Which path Tailscale uses

Tailscale connects two devices directly when it can and goes through a relay server (DERP) when it can't. A relay adds delay and has less bandwidth. This shows which one you're on:

```bash
tailscale ping <name of the other device>
```

A direct connection answers with `via <address>:<port>`. A relayed one reports `via DERP(<region>)`. The first answers can go through the relay while Tailscale is still working out a direct path. Look at a few. `tailscale status` also lists each device as `direct` or `relay`.

If it never leaves DERP a stricter router on either side is usually why. Tailscale's documentation on connection types covers what helps.

### Not tried yet

These haven't been tried on real links yet and there are no numbers for them:

- how much slower it gets on a DERP relay and which quality works well there
- how **Dynamic** copes with a slow home or mobile connection
- whether **UDP on Wi-Fi** helps or hurts over Tailscale
- the phone switching between mobile data and Wi-Fi mid-stream
- Browser camera over a `100.x` address
