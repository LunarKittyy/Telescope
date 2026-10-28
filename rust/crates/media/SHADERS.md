# Shader steps

A plugin that changes the picture on the graphics card hands over a `ShaderStep` (in `core::frame`): WGSL source, its uniform values as JSON and any extra pictures it samples. Lua and WASM plugins can only do it this way. The media crate builds the shader the first time a frame reaches it and runs it in the plugin's frame stage, after the steps of earlier stages and before later ones.

## What the WGSL has to contain

One function:

```wgsl
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = textureSample(src, src_sampler, uv);
    return vec4<f32>(1.0 - c.rgb, 1.0);
}
```

`uv` runs from (0, 0) at the top left to (1, 1) at the bottom right. The result is RGBA from 0 to 1 with the same gamma as the camera's picture (nothing is linearised). Values outside 0..1 are clipped and the alpha is ignored.

## What the host puts in front of it

| Name | Type | What it is |
|---|---|---|
| `src` | `texture_2d<f32>` | The picture so far, as RGBA |
| `src_sampler` | `sampler` | Linear filtering, clamped at the edges |
| `frame.size` | `vec2<f32>` | Width and height in pixels |
| `frame.time` | `f32` | Seconds since the stream started |
| `frame.number` | `u32` | The frame's number (low 32 bits), different for every frame |

These names are taken, along with `FrameInfo`, `VsOut`, `vs_main` and `fs_main`.

## Uniforms

Declare a struct called `Uniforms` and the host binds it as `u`:

```wgsl
struct Uniforms {
    strength: f32,
    tint: vec3<f32>,
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = textureSample(src, src_sampler, uv);
    return vec4<f32>(mix(c.rgb, u.tint, u.strength), 1.0);
}
```

Each field is filled from `ShaderStep.uniforms` by its name (`{"strength": 0.3, "tint": [1, 0.8, 0.6]}`). Numbers go into `f32`, `i32` and `u32` fields, `true` and `false` become 1 and 0, and vectors, matrices (column by column) and fixed-size arrays take JSON arrays. A field with no value is 0. WGSL doesn't allow `bool` in a uniform struct, so use `u32` for a switch. The values stay as they are until the plugin hands over a new step (`host.refresh_frame_step()`).

## Extra pictures

Every `(name, Frame)` in `ShaderStep.textures` becomes a `texture_2d<f32>` with that name, sampled with `src_sampler`. A LUT, a logo or a mask all work this way. They go up to the card once when the shader is built and arrive as RGBA whatever format the frame had.

## When it goes wrong

A shader that doesn't compile, has no `effect` or fails the card's checks is logged once with the compiler's message and left out for the rest of the stream. The picture goes through unchanged, the same as for a CPU step that panics.

## Cost

A run of shader steps one after another stays on the card. The frame goes up once before the first and comes back once after the last. A CPU step between two shader steps costs one more round trip. With no shader steps in a stream the card isn't touched at all.

`TELESCOPE_NO_GPU=1` keeps the card out of it for a driver that misbehaves or to compare. The preview is then drawn on the processor and shader steps are left out.
