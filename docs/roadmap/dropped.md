# Dropped ideas

Ideas that got talked through and left, with why and when. Not a promise they'll never happen. If the reason stops being true one can move back to [future.md](future.md).

## A see-through background in One picture (October 2026)

Background and Chroma key could hand their mask to One picture and put a cut-out person over another stream. The AI mask only looks fine with the blur or picture behind it hiding its rough edges though and someone with a green screen and several cameras most likely uses OBS for scenes already. Call apps throw away the alpha channel of a webcam, which rules out a transparent picture on the virtual camera itself.

## Sending the finished picture to another computer (October 2026)

A stream that an OBS Browser Source or a second Telescope could open would let an old laptop do all the processing and a Browser Source would even bring transparency into OBS. The desktop's share of the work is small next to an OBS encode. Latency, bandwidth and pairing would make it a lot of trouble for few people. Browser camera's server would be where to start.

## More than four cameras (October 2026)

The limit itself is a few constants (`vcam.MAX_SLOTS`, `composite.MAX_TILES` and `kSlots` in the Windows camera DLL). The work is in adding virtual cameras for people who already have four set up (root on Linux, a rebuilt and re-registered camera on Windows) and in laptops that can't keep up once every extra stream gets decoded and run through effects.
