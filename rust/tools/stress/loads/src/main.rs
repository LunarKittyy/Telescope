//! The moving parts stress.py can't be in Python: a GPU load, a CPU load and a timestamping virtual camera reader.
//! Each runs for --seconds and prints one line per event to stdout, stamped with CLOCK_MONOTONIC, which the app's own
//! delivery log uses too.
//!   stress-loads gpu  --seconds 30 [--width 2560] [--height 1440] [--steps 40] [--vram-mb 3000] [--slices 1]    t_ns
//!   stress-loads cpu  --seconds 30 [--threads 6] [--duty 100]                                      t_ns slices worst_us
//!   stress-loads read --seconds 30 --device /dev/video11 [--tee FIFO]                              t_ns hash

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn now_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: ts is a valid out-pointer
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn number(args: &[String], name: &str, default: u64) -> u64 {
    value(args, name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds = number(&args, "--seconds", 30);
    let until = Instant::now() + Duration::from_secs(seconds);
    let result = match args.first().map(String::as_str) {
        Some("gpu") => gpu(&args, until),
        Some("cpu") => cpu(&args, until),
        Some("read") => read(&args, until),
        _ => Err("use gpu, cpu or read".into()),
    };
    if let Err(e) = result {
        eprintln!("stress-loads: {e}");
        std::process::exit(1);
    }
}

// Draws heavy frames flat out with two in flight, so the 3D engine never runs dry, the way a game behind a frame queue doesn't
fn gpu(args: &[String], until: Instant) -> Result<(), String> {
    let (width, height) = (number(args, "--width", 2560) as u32, number(args, "--height", 1440) as u32);
    let steps = number(args, "--steps", 40) as u32;
    // Bands of the frame, each its own submission: the same work, in the pieces a real game hands the driver
    let slices = number(args, "--slices", 1).clamp(1, height as u64) as u32;
    // One layer is 2048 x 2048 x 4 = 16 MiB
    let layers = (number(args, "--vram-mb", 3000) / 16).max(1) as u32;
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    })).map_err(|e| e.to_string())?;
    eprintln!("gpu load on {}", adapter.get_info().name);
    let mut limits = wgpu::Limits::default();
    limits.max_texture_array_layers = limits.max_texture_array_layers.max(layers);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits,
        ..Default::default()
    })).map_err(|e| e.to_string())?;

    let size = wgpu::Extent3d { width: 2048, height: 2048, depth_or_array_layers: layers };
    let noise = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noise"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut bytes = vec![0u8; 2048 * 2048 * 4];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for layer in 0..layers {
        // A cheap new pattern per layer, so no driver can share the pages
        for chunk in bytes.chunks_exact_mut(8) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            chunk.copy_from_slice(&seed.to_le_bytes());
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &noise, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: layer }, aspect: wgpu::TextureAspect::All },
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(2048 * 4), rows_per_image: Some(2048) },
            wgpu::Extent3d { width: 2048, height: 2048, depth_or_array_layers: 1 },
        );
        queue.submit([]);
        device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
    }
    drop(bytes);

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let shader = device.create_shader_module(wgpu::include_wgsl!("gpu.wgsl"));
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("params"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("heavy"),
        layout: None,
        vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::TextureFormat::Rgba16Float.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let array = noise.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&array) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
        ],
    });

    let out = std::io::stdout();
    let mut out = out.lock();
    let start = Instant::now();
    let mut previous = None;
    while Instant::now() < until {
        let time = start.elapsed().as_secs_f32();
        let mut uniforms = [0u8; 16];
        uniforms[..4].copy_from_slice(&time.to_le_bytes());
        uniforms[4..8].copy_from_slice(&steps.to_le_bytes());
        uniforms[8..12].copy_from_slice(&layers.to_le_bytes());
        queue.write_buffer(&params, 0, &uniforms);
        let mut index = None;
        for slice in 0..slices {
            let mut enc = device.create_command_encoder(&Default::default());
            {
                let load = if slice == 0 { wgpu::LoadOp::Clear(wgpu::Color::BLACK) } else { wgpu::LoadOp::Load };
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &group, &[]);
                let (top, bottom) = (height * slice / slices, height * (slice + 1) / slices);
                pass.set_scissor_rect(0, top, width, bottom - top);
                pass.draw(0..3, 0..1);
            }
            index = Some(queue.submit([enc.finish()]));
        }
        let index = index.expect("at least one slice");
        // Waiting on the one before leaves this one running while the next gets built
        if let Some(index) = previous.replace(index) {
            device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: None }).map_err(|e| e.to_string())?;
            writeln!(out, "{}", now_ns()).map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

const CHASE_LEN: usize = 1 << 21;

// A fixed slice of what a game's and a browser's threads do: hashing, walking memory, float maths, sorting
struct Work {
    chase: Vec<u32>,
    bytes: Vec<u8>,
    a: Vec<f32>,
    b: Vec<f32>,
    c: Vec<f32>,
    list: Vec<u32>,
    seed: u64,
    at: u32,
}

impl Work {
    fn new(seed: u64) -> Self {
        let mut w = Work {
            chase: (0..CHASE_LEN as u32).collect(),
            bytes: vec![0; 256 * 1024],
            a: vec![1.0; 96 * 96],
            b: vec![0.5; 96 * 96],
            c: vec![0.0; 96 * 96],
            list: vec![0; 8192],
            seed: seed | 1,
            at: 0,
        };
        // Sattolo's shuffle makes a single cycle through the whole list
        for i in (1..CHASE_LEN).rev() {
            let j = (w.next() % i as u64) as usize;
            w.chase.swap(i, j);
        }
        w
    }

    fn next(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }

    fn slice(&mut self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for b in &self.bytes {
            hash = (hash ^ *b as u64).wrapping_mul(0x100_0000_01b3);
        }
        self.bytes[(hash % 1024) as usize] ^= 1;
        for _ in 0..20_000 {
            self.at = self.chase[self.at as usize];
        }
        for i in 0..96 {
            for j in 0..96 {
                let mut sum = 0.0;
                for k in 0..96 {
                    sum += self.a[i * 96 + k] * self.b[k * 96 + j];
                }
                self.c[i * 96 + j] = sum * 0.999;
            }
        }
        std::mem::swap(&mut self.a, &mut self.c);
        for i in 0..self.list.len() {
            self.list[i] = self.next() as u32;
        }
        self.list.sort_unstable();
        hash ^ self.at as u64 ^ self.list[17] as u64
    }
}

// Each thread works a slice, then rests long enough to hold the stated duty. Prints per quarter second across all threads
fn cpu(args: &[String], until: Instant) -> Result<(), String> {
    let threads = number(args, "--threads", 6);
    let duty = number(args, "--duty", 100).clamp(1, 100) as f64 / 100.0;
    let (done, worst, stop) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
    let workers: Vec<_> = (0..threads).map(|t| {
        let (done, worst, stop) = (done.clone(), worst.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut work = Work::new(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(t + 1));
            let mut sink = 0;
            while !stop.load(Ordering::Relaxed) {
                let begin = Instant::now();
                sink ^= work.slice();
                let took = begin.elapsed();
                done.fetch_add(1, Ordering::Relaxed);
                worst.fetch_max(took.as_micros() as u64, Ordering::Relaxed);
                if duty < 1.0 {
                    std::thread::sleep(took.mul_f64(1.0 / duty - 1.0));
                }
            }
            std::hint::black_box(sink);
        })
    }).collect();
    let out = std::io::stdout();
    let mut out = out.lock();
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(250));
        let (slices, longest) = (done.swap(0, Ordering::Relaxed), worst.swap(0, Ordering::Relaxed));
        writeln!(out, "{} {slices} {longest}", now_ns()).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
    }
    stop.store(true, Ordering::Relaxed);
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

const CAPTURE: u32 = 1;
const VIDIOC_G_FMT: libc::c_ulong = 0xC0D0_5604;
// Where sizeimage sits in struct v4l2_format
const SIZE_IMAGE: usize = 28;

// Hands frames to a slower reader without ever waiting for it: what it hasn't taken is overwritten by the next one
fn tee(path: String, slot: Arc<(std::sync::Mutex<Option<Vec<u8>>>, std::sync::Condvar)>) {
    let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(path) else { return };
    let mut frame;
    loop {
        {
            let mut held = slot.0.lock().unwrap();
            while held.is_none() {
                held = slot.1.wait(held).unwrap();
            }
            frame = held.take();
        }
        if frame.as_ref().is_some_and(|f| file.write_all(f).is_err()) {
            return;
        }
    }
}

// Waits for a writer, since v4l2loopback offers no capture until one has started, then stamps every frame as it arrives. The driver
// gives capture to one reader at a time, so with --tee this one stands in front of the OBS-like load and passes the frames on
fn read(args: &[String], until: Instant) -> Result<(), String> {
    let device = value(args, "--device").ok_or("--device is needed")?;
    let path = std::ffi::CString::new(device.clone()).map_err(|e| e.to_string())?;
    // SAFETY: path is a valid C string
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
    if fd < 0 {
        return Err(format!("{device}: {}", std::io::Error::last_os_error()));
    }
    let mut format = [0u8; 208];
    format[..4].copy_from_slice(&CAPTURE.to_ne_bytes());
    // SAFETY: format is the 208 bytes of a struct v4l2_format
    while unsafe { libc::ioctl(fd, VIDIOC_G_FMT as _, format.as_mut_ptr()) } != 0 {
        if Instant::now() >= until {
            return Err(format!("{device} never offered frames"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let size = u32::from_ne_bytes(format[SIZE_IMAGE..SIZE_IMAGE + 4].try_into().unwrap()) as usize;
    let mut frame = vec![0u8; size];
    let slot = Arc::new((std::sync::Mutex::new(None::<Vec<u8>>), std::sync::Condvar::new()));
    let teeing = value(args, "--tee").map(|path| {
        let slot = slot.clone();
        std::thread::spawn(move || tee(path, slot))
    });
    let out = std::io::stdout();
    let mut out = out.lock();
    while Instant::now() < until {
        // SAFETY: frame has size writable bytes
        let got = unsafe { libc::read(fd, frame.as_mut_ptr().cast(), size) };
        let stamp = now_ns();
        if got <= 0 {
            eprintln!("read: {got} {}", std::io::Error::last_os_error());
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        // Every 127th byte tells two frames of a noisy picture apart for a tiny part of a whole-frame hash
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in frame[..got as usize].iter().step_by(127) {
            hash = (hash ^ *byte as u64).wrapping_mul(0x100_0000_01b3);
        }
        writeln!(out, "{stamp} {hash:x}").map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
        if teeing.is_some() {
            *slot.0.lock().unwrap() = Some(frame[..got as usize].to_vec());
            slot.1.notify_one();
        }
    }
    Ok(())
}
