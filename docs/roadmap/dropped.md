# Dropped ideas

Ideas that got talked through and left, with why and when. Not a promise they'll never happen. If the reason stops being true an idea can move back to [future.md](future.md).

## Sending the finished picture to another computer (October 2026)

A stream that an OBS Browser Source or a second Telescope could open would let an old laptop do all the processing and a Browser Source would even bring transparency into OBS. The desktop's share of the work is small next to an OBS encode. Latency, bandwidth and pairing would make it a lot of trouble for few people. Browser camera's server would be where to start.

## More than four cameras (October 2026)

The limit itself is a few constants (`vcam.MAX_SLOTS`, `composite.MAX_TILES` and `kSlots` in the Windows camera DLL). The work is in adding virtual cameras for people who already have four set up (root on Linux, a rebuilt and re-registered camera on Windows) and in laptops that can't keep up once every extra stream gets decoded and run through effects.

## The decoder in a child process (November 2026)

The idea was to run the video decoder in a separate process so hardware decoding could return without a game on the graphics card stalling the app. It didn't fix the stall. The stall came from the driver's I420 conversion queuing behind the game's work. Asking the card for NV12 and splitting it on the processor fixed that. Hardware decoding is on by default with VA-API again without it.

## Overlapping Spout's copy and conversion (November 2026)

The idea was to copy the next Spout frame while the last one is converted. Capture already does that, since a reader thread fetches the next picture while a decoder thread converts the last. A benchmark that showed a gain had compared against a version that ran the two one after the other.

## One working frame and effects on the YUV planes (November 2026)

Two ideas were meant to cut the cost of steps on the processor. They were converting a frame to RGB once for the whole chain and running brightness and colour steps on the YUV planes. Measured at 4K, the steps left on the processor cost about 1 ms each, too little to be worth the change. Two more ideas went the same way. The copy of a decoded frame and the BT.709 conversion would each save under 0.5 ms. Keeping the picture in RGB between filters was measured too. The round trip only costs about 0.5 ms a filter at 1080p, so three filters ran 3 to 6 % faster and five filters ran 2 to 3 % slower.

## Steps on the integrated graphics card (November 2026)

The idea was to move steps onto a laptop's integrated card while a game has the main card busy. A frame can't go straight from one card to the other. Each move costs the same trip through memory as falling back to the processor. The integrated card is also slower and shares memory bandwidth with the processor. Most steps left on the processor only cost about 1 ms each. Worth a quick measurement if someone with a busy laptop finds the processor fallback too slow.

## H.265 (November 2026)

A spike looked at sending H.265 from phones that support it. Everything on both sides of the video path is written for H.264, from the phone's encoder to the headers the desktop reads. That makes it a rewrite of about a dozen files on two platforms. The gain is mostly bitrate, which only matters on weak Wi-Fi where Dynamic already steps down. Phones' H.265 encoders also differ a lot and need testing on real ones.

## Auto colour grading: whole look and always on (November 2026)

Two extras were left over from Auto grade. One was a whole-look button that also turns on auto framing and a light background blur. The other was an always-on mode that keeps grading as the light changes. Auto grade is there already and it isn't going further for now. The always-on mode would also have needed heavy smoothing to stop the picture from pumping.

## Colour matching between phones and 10-bit (November 2026)

The idea was to pick the phone you like best and bring the others close to it, fitted from a printed sheet with grey and colour patches and applied as a colour matrix and a curve for each channel. The other half was 10-bit HLG or log from phones that offer it, for more room under heavy grades. 3D's Match already does a simple version of the matching for two phones. 10-bit isn't worth it. Virtual cameras and the apps that read them are 8-bit, phones mostly give 10-bit as H.265 (which is on this list too) and the whole desktop chain is 8-bit. The gain is small next to that work.

## Going back to the Python version and downgrades in the updater (November 2026)

Settings had a "Go back to the Python version" button pinned to 4.4.2, which Rust needed before it reached the stable channel. Enough time has passed since 5.0, so the button and the code behind it are removed. Downgrades in the updater in general were to follow it and are dropped with it.

## The phone's screen timing out and a lower capture rate after Dynamic (November 2026)

Two ideas for the phone app, to save battery. One was letting the idle screen time out and releasing the wake lock. The other was lowering the camera's capture rate after a Dynamic cap. Many phones kill apps while the screen is off whatever the settings say, so Wait for the computer only works there with the screen kept on, and the wake lock stays on purpose. A lower capture rate makes the picture hitch. Dynamic's cap stays in the encoder.

## Whiteboard detection (November 2026)

The idea was for Telescope to find a whiteboard's corners itself, follow it as the phone moves and even out shadows and glare. It's big and most of the work would be tuning against real rooms for a small group of people. Desk mode already covers marking the corners by hand. Using presets as scenes already works, which left nothing else of the idea.

## AI models in plugins (November 2026)

The idea was for a plugin to bundle a small `.onnx` model and run it as a frame stage. A GPU ONNX runtime would make the downloads much bigger while the app is trying to get smaller and is hard to support across platforms.

## Live captions (November 2026)

A plugin that wrote captions from the microphone with Whisper or Vosk. Call apps already caption. It would also have needed a second optional download.

## Spout capture with zero-copy (November 2026)

The idea was to hand a Spout sender's GPU buffer straight to the picture work. The frame would never leave the card. It's big for a small group and no measurement shows the copy is a bottleneck.

## Undo across the app (November 2026)

One undo and redo for everything, from Ctrl+Z or a button in the header. Each card's Reset and presets cover most of what people would use it for. It would also have needed a rule for what counts as one step in every plugin.

## The front of the chain on the phone (November 2026)

Doing the first effect steps on the phone to take load off the desktop. The phone is where heat already limits the app.
