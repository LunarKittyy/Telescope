<!-- covers: rust/crates/link/src/net.rs rust/crates/link/src/pairing.rs rust/crates/link/src/phones.rs android/app/src/main/kotlin/com/telescope/Pairing.kt; checked: d369c8664 -->
# Unusual setups

Telescope is built around a phone and a computer on the same Wi-Fi. It doesn't care where either of them is as long as they can reach each other. This page is for the cases where they aren't in the same room. It starts with Tailscale, since that's the easiest way to get there.

## Tailscale

[Tailscale](https://tailscale.com) puts your devices on one private network (a tailnet) with an address like `100.x.y.z` each, wherever they are. Telescope already knows about those addresses and there is nothing to switch on. Put the phone and the computer on the same tailnet and pair as usual.

How it works with Telescope:

- The QR code lists the computer's addresses, LAN ones first and the Tailscale one after. The phone tries the LAN ones over Wi-Fi first, then every address on whatever network it has. Away from home that ends on the Tailscale address. Each attempt gets 2 seconds and the whole run 12.
- The computer remembers all of the phone's addresses. Later it tries the phone's address it found on the local network, then the last one that worked, then the rest with Tailscale first.
- Pairing needs the **Add phone** dialog open on the computer, because that's the only time its pairing server on port 8765 runs. The phone has to reach the computer for this. After that the computer reaches out to the phone.
- If you use Tailscale access rules, the phone needs to reach the computer on 8765 while pairing. The computer needs to reach the phone on 8766 and 8080. [Browser camera](features/sources.md#browser-camera) uses 8767 on the computer. Tailscale's default rules allow all of this.

To pair, install Tailscale on the phone, sign in to the same tailnet and open Telescope on both as usual. Then do **Add phone** and scan the code. The stream, the effects and the virtual camera all work the same.

### Someone else's phone as a camera

A friend joins your tailnet (Tailscale can share a single machine with them so they don't get the rest of your devices), pairs their phone with your Telescope and shows up in OBS as a remote guest. It's a direct camera feed instead of a screen-captured video call and looks a lot sharper.

The QR code carries the token that lets a phone pair. Treat it like a password and only show it to the person who should scan it. Close **Add phone** once they have. One code pairs one phone. When the guest is done, remove their phone on the computer and Telescope tells the phone to forget the computer.

### A phone you leave somewhere

A phone at the cottage, or a stargazer rig in the garden, that you watch from your desk. Pair it once while you're there, or over the tailnet if you can reach it, and it works from anywhere after that. **Wait for my computer** in the phone app lets you start the camera from your computer without touching the phone, which is the point when it's out of reach.

Plug it into power. Some phones still close apps in the background. Check that Telescope survives a night on yours before relying on it.

### Helping someone remotely

You can pair your phone with their Telescope the same way and use your camera to show them something. They open **Add phone**, you scan the code (they have to show it to you, so the warning above applies) and you remove your phone from their list when you're done.

## Which path Tailscale uses

Tailscale connects two devices directly when it can and goes through a relay server (DERP) when it can't. A relay adds delay and has less bandwidth. This shows which one you're on:

```bash
tailscale ping <name of the other device>
```

A direct connection answers with `via <address>:<port>`. A relayed one reports `via DERP(<region>)`. The first answers can go through the relay while Tailscale is still working out a direct path. Look at a few. `tailscale status` also lists each device as `direct` or `relay`.

When it never leaves DERP, a stricter router on either side is usually why. Tailscale's documentation on connection types covers what helps.

## Not tried yet

These haven't been tried on real links yet and there are no numbers for them:

- how much slower it gets on a DERP relay and which quality works well there
- how **Dynamic** copes with a slow home or mobile connection
- whether **UDP on Wi-Fi** helps or hurts over Tailscale
- the phone switching between mobile data and Wi-Fi mid-stream
- Browser camera over a `100.x` address
