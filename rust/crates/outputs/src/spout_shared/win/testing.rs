//! What the Windows tests build their pictures and devices with.

use super::*;
use ::windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use ::windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA;

/// A device on the software rasteriser, which every Windows machine has, whatever its graphics card (or a VM's lack of one).
pub fn warp() -> Gpu {
    let levels: [D3D_FEATURE_LEVEL; 3] = [D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
    let (mut device, mut context) = (None, None);
    // SAFETY: the out pointers are valid Options and no adapter is given
    unsafe {
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE(std::ptr::null_mut()), D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&levels), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context))
            .expect("a WARP device");
    }
    Gpu { device: device.expect("a device"), context: context.expect("a context"), adapter: "WARP".into() }
}

/// Every device these tests can try: WARP, and the machine's own graphics card when it has one.
pub fn devices() -> Vec<Gpu> {
    let mut all = vec![warp()];
    match Gpu::new(None) {
        Ok(card) => all.push(card),
        Err(why) => println!("no graphics card to test on: {why}"),
    }
    all
}

/// A texture of `format` (a DXGI number) holding `data`, as if a sender had shared it; bound to nothing, so the converter needs a copy of its own.
pub fn texture_with(gpu: &Gpu, (width, height): (u32, u32), format: u32, data: &[u8]) -> SharedTexture {
    assert_eq!(data.len(), width as usize * height as usize * 4);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT(format as i32),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: 0,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let initial = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as *const c_void, SysMemPitch: width * 4, SysMemSlicePitch: 0 };
    let mut made: Option<ID3D11Texture2D> = None;
    // SAFETY: desc and initial are valid for the call, data holds the `height` rows of `width * 4` bytes that initial says, and made is a valid out Option
    unsafe { gpu.device.CreateTexture2D(&desc, Some(&initial), Some(&mut made)) }.expect("a texture");
    SharedTexture { texture: made.expect("a texture"), width, height, format, keyed: false }
}

/// Bytes that run through every channel value, the same for the same seed.
pub fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(12_345);
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

/// Pixels of the colours a picture may hold the most of: black, white, the primaries and their mixes, then noise.
pub fn extremes_then_noise(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let corners: Vec<[u8; 4]> = (0..8u8).map(|i| [(i & 1) * 255, ((i >> 1) & 1) * 255, ((i >> 2) & 1) * 255, 255]).collect();
    let mut data = noise(width as usize * height as usize * 4, seed);
    for (i, pixel) in data.as_chunks_mut::<4>().0.iter_mut().take(width as usize * height as usize / 2).enumerate() {
        pixel.copy_from_slice(&corners[(i / 3) % 8]);
    }
    data
}
