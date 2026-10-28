# Next release

Planned in October 2026 after 4.4. PyAV 19 is left over from 4.3.

- [ ] [PyAV 19](#pyav-19)

## PyAV 19

Needs Python 3.12, which 4.2 brought in. aiortc 1.15 still declares `av<18` which we get around with `--no-deps`. aiortc's main branch already allows `av<20` but there's no release with it yet. Wait for that release, then drop the `--no-deps` workaround if it's no longer needed. PyAV 19 has API changes too. Check browser video and audio and hardware decoding on a real device. The draft PR is #273.
