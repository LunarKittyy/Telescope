# Shader steps

What a plugin's WGSL contains and what it gets (`fn effect`, `sample()`, `frame`, `struct Params`, extra textures) is in `core/src/shader.rs`, shared with plugin-host and its plugin guide. This page only covers how the media crate runs it.

## When it goes wrong

The executor builds a step the first time a frame reaches it. A shader that `shader::assemble` refuses, whose `Params` don't match its uniforms, or that fails naga's checks or the card's is logged once with the message and left out for the rest of the stream. The picture goes through unchanged, the same as for a CPU step that panics.

## Cost

A run of shader steps one after another stays on the card. The frame goes up once before the first and comes back once after the last, in the format it arrived in (I420 from the decoders). A CPU step between two shader steps costs one more round trip. A stream with no shader steps doesn't touch the card at all. Extra textures go up once, when the step is built.

`TELESCOPE_NO_GPU=1` keeps the card out of it for a driver that misbehaves or to compare. The preview is then drawn on the processor and shader steps are left out.
