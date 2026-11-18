# Dropped ideas

Ideas that got talked through and left, with why and when. Not a promise they'll never happen. If the reason stops being true an idea can move back to [future.md](future.md).

## A see-through background in One picture (October 2026)

Background and Chroma key could hand their mask to One picture and put a cut-out person over another stream. The AI mask only looks fine with the blur or picture behind it hiding its rough edges though and someone with a green screen and several cameras most likely uses OBS for scenes already. Call apps throw away the alpha channel of a webcam, which rules out a transparent picture on the virtual camera itself.

## Sending the finished picture to another computer (October 2026)

A stream that an OBS Browser Source or a second Telescope could open would let an old laptop do all the processing and a Browser Source would even bring transparency into OBS. The desktop's share of the work is small next to an OBS encode. Latency, bandwidth and pairing would make it a lot of trouble for few people. Browser camera's server would be where to start.

## More than four cameras (October 2026)

The limit itself is a few constants (`vcam.MAX_SLOTS`, `composite.MAX_TILES` and `kSlots` in the Windows camera DLL). The work is in adding virtual cameras for people who already have four set up (root on Linux, a rebuilt and re-registered camera on Windows) and in laptops that can't keep up once every extra stream gets decoded and run through effects.

## The decoder in a child process (November 2026)

The idea was to run the video decoder in a separate process so hardware decoding could return without a game on the graphics card stalling the app. It didn't fix the stall. The stall came from the driver's I420 conversion queuing behind the game's work. Asking the card for NV12 and splitting it on the processor fixed that. Hardware decoding is on by default with VA-API again without it.

## Overlapping Spout's copy and conversion (November 2026)

The idea was to copy the next Spout frame while the last one is converted. Capture already does that, since a reader thread fetches the next picture while a decoder thread converts the last. A benchmark that showed a gain had compared against a version that ran the two one after the other.

## One working frame and effects on the YUV planes (November 2026)

Two ideas were meant to cut the cost of steps on the processor. They were converting a frame to RGB once for the whole chain and running brightness and colour steps on the YUV planes. Measured at 4K, the steps left on the processor cost about 1 ms each, too little to be worth the change. Two more ideas went the same way. The copy of a decoded frame and the BT.709 conversion would each save under 0.5 ms.

## The rest of load balancing (November 2026)

Moving steps to the integrated graphics card needs a second graphics device and changes in how the picture work runs. Doing the front of the chain on the phone needs a pass on the phone, a protocol message and testing on real phones from several makers. Neither is planned. The graphics card choice in Settings already lets someone put everything on the integrated card.

## H.265 (November 2026)

A spike looked at sending H.265 from phones that support it. Everything on both sides of the video path is written for H.264, from the phone's encoder to the headers the desktop reads. That makes it a rewrite of about a dozen files on two platforms. The gain is mostly bitrate, which only matters on weak Wi-Fi where Dynamic already steps down. Phones' H.265 encoders also differ a lot and need testing on real ones.
