<!-- covers: rust/crates/link/src/net.rs rust/crates/link/src/pairing.rs rust/crates/link/src/phones.rs android/app/src/main/kotlin/com/telescope/Pairing.kt rust/crates/effects/src/stargazer* rust/crates/effects/src/phototrap* rust/crates/effects/src/composite* rust/crates/effects/src/fun_*; checked: d369c8664 -->
# Things to try with Telescope

A phone camera on your computer is only the start. Here are a few setups to try, some useful and some for fun. Each one is a few steps. The optional plugins are switched on under **Plugins** in Settings and need a restart, see [Switching plugins on and off](features/plugins.md#switching-plugins-on-and-off).

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

## Use an old DSLR as a webcam

A camera you already own can be a very good webcam.

1. Plug it in. A camera with a USB webcam mode shows up in the device picker as it is. One with a clean HDMI output works through an HDMI capture card, since the computer sees the card as a webcam.
2. An older DSLR that only does live view over USB also shows up in the picker, as a small picture (a Nikon D5100 sends 640×426). On Windows a Nikon or Canon does this too. That's new and reports are welcome.
3. Pick it and open the **Webcam** card for what it lets you change, like white balance, focus and aperture.

Every effect works on it like on a phone. More in [Webcams and capture cards](features/sources.md#webcams-and-capture-cards).

## Two cameras in one picture

For an app that takes only one camera, **One picture** puts your face big with the desk small in a corner, two phones side by side or a grid of up to four.

1. Switch on **One picture** under **Streaming and devices** and restart.
2. Start two streams, a second phone or a webcam with **+**.
3. Open the **One picture** card and press **Edit picture...**. **Start from** lays them out **Side by side**, **Big and small** or in a **Grid**, then drag to tweak.
4. Pick the **Telescope** camera in your call or in OBS.

More in [Several cameras in one picture](features/sources.md#several-cameras-in-one-picture).

## Try the Fun pack

The Fun pack is a few effects that don't take themselves seriously.

1. Switch on **Fun pack** under **Extras** and restart.
2. Open its card and pick an **Effect** like **Clone party**, **Invisibility cloak** or **1998 webcam**.
3. For **Clone party** wait out the three second countdown, then move and add another, up to eight. For **Invisibility cloak** take the empty room first, then step back into the picture.

The rest are in [Fun pack](features/effects.md#fun-pack).

## Send a game or a 3D scene to your call

Spout capture picks up programs that share their picture through Spout, like games with a stream camera, VTuber programs and 3D tools and sends them out on the virtual camera. It's in Lab too.

1. Switch on **Spout capture** under **Streaming and devices** and restart.
2. Start the program you want to share.
3. Pick it in the device picker under the name it shares itself with. Every effect works on it.

On Linux Spout only exists inside Proton and [Spout2PW](https://github.com/tasokait/spout2pw) passes the senders out to PipeWire. More in [Spout capture](features/sources.md#spout-capture).

## Use a phone from far away

Telescope doesn't care where the phone is as long as the two can reach each other. [Tailscale](https://tailscale.com) is the easy way to get there, since it gives every device an address that works from anywhere and Telescope already knows how to use it.

1. Install Tailscale on the phone and the computer and sign in to the same tailnet.
2. Open Telescope on both and press **Add phone**.
3. Scan the code. The stream, the effects and the virtual camera work the same.

It works for a friend's phone as a guest camera in your OBS, a phone left at the cottage or your own phone showing someone something at their place. The details are next.

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
