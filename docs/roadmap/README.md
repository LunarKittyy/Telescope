# Roadmap

A place for ideas and plans that don't need an issue. Issues stay for what users run into and ask for, while this folder holds what's planned next, what might happen some day, and a short history of what each release set out to do.

Each file covers the work that led up to one release. Once a release ships its file stays as a record of that time and doesn't get edited again. It doesn't link to next.md or to later releases (those don't exist yet when it's written), and anything that didn't make it moves on to the next file.

## Planned

- [Next release](next.md): what's planned for the release that comes next, and what's already merged for it
- [Some day](future.md): ideas worth keeping that aren't scheduled
- [Dropped ideas](dropped.md): ideas that got talked through and left, with why

## Past releases

The files up to 3.5 were written afterwards from the release notes, the commit history and the issues people opened, so they're shorter than the notes and say more about why.

- [4.4](4.4.md) (October 2026): webcams and capture cards as sources, a Black screen button on the phone and UDP on Wi-Fi moved to Settings
- [4.3](4.3.md) (October 2026): thirteen optional plugins, a Windows 11 camera every app can see, mic noise reduction and Dynamic over UDP
- [4.2](4.2.md) (October 2026): the phone app in the desktop's look, a Live Update while streaming, 720p when the phone gets very hot and a faster first start on Windows
- [4.1](4.1.md) (October 2026): both apps in six more languages and cheaper Background and Auto framing with a Power-saving effects switch
- [4.0](4.0.md) (October 2026): a lighter desktop, the new look with one Settings window, and Auto framing and Background as the first optional plugins
- [3.5](3.5.md) (October 2026): plugins, and taking the desktop app apart to make them possible
- [3.4](3.4.md) (October 2026): keyboard shortcuts, the Windows installer and Low light
- [3.3](3.3.md) (October 2026): several cameras at once, Browser camera and mic only
- [3.2](3.2.md) (October 2026): a phone mic that hears you from across the desk, with a meter, mute, gain and a limiter
- [3.1](3.1.md) (September and October 2026): Dynamic bitrate and noticing when the stream can't keep up
- [3.0](3.0.md) (September 2026): the microphone, automatic streaming, updates and encryption
- [2.x](2.x.md) (June to August 2026): from PhoneCam to Telescope, the modular rewrite and the first camera controls

## Adding to it

Put an idea in [future.md](future.md) when it's worth remembering but nobody is about to work on it, and in [next.md](next.md) once it's planned for the next release. An idea that got talked through and left goes in [dropped.md](dropped.md) with the reason. Say what the problem is, where in the code it would land and roughly how big it is. Tick it off in next.md when it's merged. A user's issue that turns into a plan can be linked from here, but it stays open on GitHub until the fix ships.

When a release is tagged, `next.md` is renamed after it (for example `3.6.md`), a line goes into the list above and a fresh `next.md` starts with whatever didn't make it.
