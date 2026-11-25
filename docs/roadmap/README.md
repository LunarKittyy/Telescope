# Roadmap

A place for ideas and plans that don't need an issue. Issues are for what users run into and ask for. This folder contains what's planned next, what might happen some day and a short history of what each release set out to do.

Each file covers the work that led up to one release. Once a release ships its file is left as a record of that time and isn't edited again. It doesn't link to next.md or to later releases (those don't exist yet when it's written). Anything that didn't make it moves on to the next file.

## Planned

- [Next release](next.md) covers what's planned for the release that comes next and what's already merged for it
- [Some day](future.md) lists ideas worth keeping that aren't scheduled
- [Dropped ideas](dropped.md) lists ideas that got talked through and left, with why

## Past releases

The files up to 3.5 were written afterwards from the release notes, the commit history and the issues people opened. They're shorter than the notes but explain more about why.

- [5.2](5.2.md) (November 2026) had Stargazer and Phototrap, 3D in a headset, one picture as a scene editor, a graphics card choice and a lighter, faster app
- [5.1](5.1.md) (November 2026) had both apps redone, about a third of the processor with every effect on, and Filters, Stats and an MCP server
- [5.0](5.0.md) (November 2026) had the desktop app rewritten in Rust, plugins in Lua, going back to Python and an AppImage
- [4.4](4.4.md) (October 2026) had webcams and capture cards as sources, a Black screen button on the phone and UDP on Wi-Fi moved to Settings
- [4.3](4.3.md) (October 2026) had thirteen optional plugins, a Windows 11 camera every app can see, mic noise reduction and Dynamic over UDP
- [4.2](4.2.md) (October 2026) had the phone app in the desktop's look, a Live Update while streaming, 720p when the phone gets very hot and a faster first start on Windows
- [4.1](4.1.md) (October 2026) had both apps in six more languages and cheaper Background and Auto framing with a Power-saving effects switch
- [4.0](4.0.md) (October 2026) had a lighter desktop, the new look with one Settings window, and Auto framing and Background as the first optional plugins
- [3.5](3.5.md) (October 2026) had plugins and taking the desktop app apart to make them possible
- [3.4](3.4.md) (October 2026) had keyboard shortcuts, the Windows installer and Low light
- [3.3](3.3.md) (October 2026) had several cameras at once, Browser camera and mic only
- [3.2](3.2.md) (October 2026) had a phone mic that hears you from across the desk, with a meter, mute, gain and a limiter
- [3.1](3.1.md) (September and October 2026) had Dynamic bitrate and noticing when the stream can't keep up
- [3.0](3.0.md) (September 2026) had the microphone, automatic streaming, updates and encryption
- [2.x](2.x.md) (June to August 2026) went from PhoneCam to Telescope, the modular rewrite and the first camera controls

## Adding to it

Put an idea in [future.md](future.md) when it's worth remembering but nobody is about to work on it and in [next.md](next.md) once it's planned for the next release. An idea that got talked through and left goes in [dropped.md](dropped.md) with the reason. Describe the problem, where in the code it would land and roughly how big it is. Tick it off in next.md when it's merged. A user's issue that turns into a plan can be linked from here but is still open on GitHub until the fix ships.

Tagging a release renames `next.md` after it (for example `3.6.md`), adds a line to the list above and starts a fresh `next.md` with whatever didn't make it.
