# Next release

The next release follows 5.0.

- [ ] [Auto colour grading](#auto-colour-grading)

## Auto colour grading

A button on the Colour grading card for people who don't want to set the grade up themselves. It measures about a second of frames and fills in the existing sliders to give a starting point anyone can still tweak instead of a black box. A cheap laptop camera with a hand-made grade, auto framing and a light background blur already looks like a proper webcam, and this should get most people there with one press.

What it would measure, all as numbers from the frames:

- White balance from the face, not the whole frame because a coloured wall throws off the usual "make the average grey" approach. Skin of every tone sits close to one hue line and Temperature and Tint turn the face's average colour onto it.
- Gamma so the face lands around middle brightness and Gain so the brightest part of the background doesn't clip.
- Lift and Contrast from the histogram, so the darkest bit sits just above black while a flat picture gets some depth.
- Highlights from how much of the frame is close to clipping, like a window or a lamp behind you.
- Saturation towards a target, held back by the skin so faces never go orange.

Later, maybe a whole-look button that also turns on auto framing and a light background blur, and an always-on mode. That one would need heavy smoothing to stop the picture from pumping when the light changes.
