//! Where the worker writes: the Linux pipe source's FIFO, or VB-Cable's playback end on Windows (audio.py's FifoSink
//! and SoundDeviceSink).

#[cfg(unix)]
pub use fifo::FifoSink;
#[cfg(windows)]
pub use windows::{output_devices, SoundDeviceSink};

#[cfg(unix)]
mod fifo {
    use crate::jitter::{monotonic, RATE};
    use crate::worker::MicSink;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// About 40 ms; Linux won't go below a page.
    pub const PIPE_BYTES: i32 = 4096;
    /// A pipe that stays full this long has no reader (nothing uses the mic): drop the chunk.
    pub const STALL: Duration = Duration::from_millis(200);
    #[cfg(target_os = "linux")]
    const F_SETPIPE_SZ: libc::c_int = 1031;

    /// The virtual mic's FIFO. The pipe source reads whatever arrives, so this paces itself to real time, and the pipe
    /// is shrunk to one page so a stalled reader can't queue up delay.
    pub struct FifoSink {
        fd: libc::c_int,
        clock: Box<dyn FnMut() -> f64 + Send>,
        sleep: Box<dyn FnMut(f64) + Send>,
        due: Option<f64>,
        pub stall: Duration,
    }

    impl FifoSink {
        pub fn open(path: &Path) -> io::Result<FifoSink> {
            Ok(FifoSink::with_fd(open_fifo(path)?, Box::new(monotonic), Box::new(|s| std::thread::sleep(Duration::from_secs_f64(s)))))
        }

        /// Takes over fd (it's closed with the sink) and makes it non-blocking, so a pipe nobody reads can't hold up
        /// stopping.
        pub fn with_fd(fd: libc::c_int, clock: Box<dyn FnMut() -> f64 + Send>, sleep: Box<dyn FnMut(f64) + Send>) -> FifoSink {
            // SAFETY: fcntl on a descriptor this sink owns
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            FifoSink { fd, clock, sleep, due: None, stall: STALL }
        }

        pub fn write_bytes(&mut self, data: &[u8]) -> io::Result<()> {
            let now = (self.clock)();
            match self.due {
                Some(due) if now - due <= 0.05 => {
                    if due > now {
                        (self.sleep)(due - now);
                    }
                }
                _ => self.due = Some(now), // first write, or so far behind that catching up would only burst
            }
            // Chunks are under PIPE_BUF, so each write goes in whole or not at all and samples never split
            let mut give_up: Option<Instant> = None;
            loop {
                // SAFETY: data is valid for its length and fd is this sink's
                let n = unsafe { libc::write(self.fd, data.as_ptr().cast(), data.len()) };
                if n >= 0 {
                    break;
                }
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::WouldBlock {
                    if e.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(e);
                }
                let until = *give_up.get_or_insert_with(|| Instant::now() + self.stall);
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                let mut pfd = libc::pollfd { fd: self.fd, events: libc::POLLOUT, revents: 0 };
                // SAFETY: one pollfd, waiting at most left
                let ready = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as libc::c_int) };
                if ready <= 0 {
                    break;
                }
            }
            self.due = Some(self.due.unwrap_or(now) + data.len() as f64 / (RATE * 2) as f64);
            Ok(())
        }
    }

    impl MicSink for FifoSink {
        fn write(&mut self, samples: &[i16]) -> io::Result<()> {
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            self.write_bytes(&bytes)
        }
    }

    impl Drop for FifoSink {
        fn drop(&mut self) {
            // SAFETY: closed once, here
            unsafe { libc::close(self.fd) };
        }
    }

    /// Non-blocking open fails at once if nothing holds the read end, instead of hanging. Only the sound server's pipe,
    /// made by this user: never a file someone left there to have audio written into.
    fn open_fifo(path: &Path) -> io::Result<libc::c_int> {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: a valid C string
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstat into a zeroed struct on the fd just opened
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::fstat(fd, &mut st) } == 0
            && (st.st_mode & libc::S_IFMT) == libc::S_IFIFO
            && st.st_uid == unsafe { libc::getuid() };
        if !ok {
            // SAFETY: closing what was opened above
            unsafe { libc::close(fd) };
            return Err(io::Error::other(format!("{} isn't the microphone's pipe", path.display())));
        }
        #[cfg(target_os = "linux")]
        // SAFETY: a pipe's size; failing is fine
        unsafe {
            libc::fcntl(fd, F_SETPIPE_SZ, PIPE_BYTES);
        }
        Ok(fd)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::{Arc, Mutex};

        fn pipe() -> (libc::c_int, libc::c_int) {
            let mut fds = [0; 2];
            // SAFETY: two ints for pipe()
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
            (fds[0], fds[1])
        }

        fn read_all(fd: libc::c_int) -> Vec<u8> {
            let mut buf = vec![0u8; 1 << 20];
            // SAFETY: reading into a buffer of its size
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            buf.truncate(n.max(0) as usize);
            buf
        }

        fn scratch(name: &str) -> std::path::PathBuf {
            let d = std::env::temp_dir().join(format!("telescope-fifo-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        #[test]
        fn fifo_sink_writes_everything_at_real_time_pace() {
            let (read_end, write_end) = pipe();
            let clock = Arc::new(Mutex::new(100.0f64));
            let sleeps = Arc::new(Mutex::new(Vec::<f64>::new()));
            let (c1, c2, s2) = (clock.clone(), clock.clone(), sleeps.clone());
            let mut sink = FifoSink::with_fd(write_end, Box::new(move || *c1.lock().unwrap()), Box::new(move |s| {
                s2.lock().unwrap().push((s * 10_000.0).round() / 10_000.0);
                *c2.lock().unwrap() += s;
            }));
            let ten_ms = vec![0u8; 960];
            for _ in 0..3 {
                sink.write_bytes(&ten_ms).unwrap();
            }
            assert_eq!(read_all(read_end), [ten_ms.clone(), ten_ms.clone(), ten_ms.clone()].concat());
            assert_eq!(*sleeps.lock().unwrap(), vec![0.01, 0.01]); // the first write goes at once, then one every 10 ms
            *clock.lock().unwrap() += 1.0; // a long stall: start over rather than burst to catch up
            sink.write_bytes(&ten_ms).unwrap();
            assert_eq!(*sleeps.lock().unwrap(), vec![0.01, 0.01]);
            drop(sink);
            // SAFETY: closing the test's own read end
            unsafe { libc::close(read_end) };
        }

        #[test]
        fn fifo_sink_fails_fast_when_the_source_is_gone() {
            let dir = scratch("gone");
            assert!(FifoSink::open(&dir.join("missing")).is_err());
            let path = dir.join("f"); // a FIFO nobody reads: the open must fail rather than hang
            let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: a valid C string
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
            assert!(FifoSink::open(&path).is_err());
        }

        #[test]
        fn fifo_sink_refuses_a_file_or_a_link_in_place_of_the_pipe() {
            let dir = scratch("refuse");
            let target = dir.join("precious.txt");
            std::fs::write(&target, "keep me").unwrap();
            assert!(FifoSink::open(&target).is_err());
            let link = dir.join("mic.fifo");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(FifoSink::open(&link).is_err());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn fifo_sink_drops_audio_nobody_reads_instead_of_blocking() {
            let (read_end, write_end) = pipe();
            // SAFETY: shrinking the test's own pipe
            unsafe { libc::fcntl(write_end, F_SETPIPE_SZ, PIPE_BYTES) };
            let mut sink = FifoSink::with_fd(write_end, Box::new(monotonic), Box::new(|s| std::thread::sleep(Duration::from_secs_f64(s))));
            sink.stall = Duration::from_millis(20);
            let started = Instant::now();
            for _ in 0..20 {
                // five times what the pipe holds
                sink.write_bytes(&[0u8; 960]).unwrap();
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            assert_eq!(read_all(read_end).len() % 960, 0); // what went in went in whole
            drop(sink);
            // SAFETY: closing the test's own read end
            unsafe { libc::close(read_end) };
        }
    }
}

#[cfg(windows)]
mod windows {
    use crate::jitter::RATE;
    use crate::worker::MicSink;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    /// What may wait for the output before a write blocks (about 40 ms, like the Linux pipe).
    const QUEUE: usize = RATE * 40 / 1000;
    /// An output that takes nothing for this long isn't playing: drop the chunk.
    const STALL: Duration = Duration::from_millis(200);

    /// Every output device with how many channels it plays, for find_vb_cable.
    pub fn output_devices() -> Vec<(String, u32)> {
        let Ok(devices) = cpal::default_host().output_devices() else { return Vec::new() };
        devices
            .map(|d| {
                let name = d.description().map(|desc| desc.name().to_string()).unwrap_or_default();
                let channels = d.supported_output_configs().map(|c| c.map(|r| r.channels() as u32).max().unwrap_or(0)).unwrap_or(0);
                (name, channels)
            })
            .collect()
    }

    type Queue = Arc<(Mutex<VecDeque<i16>>, Condvar)>;

    /// VB-Cable's playback end through the system's sound output (WASAPI).
    pub struct SoundDeviceSink {
        queue: Queue,
        _stream: cpal::Stream,
    }

    // SAFETY: the stream is only kept so it plays until the sink goes; it's never used from another thread
    unsafe impl Send for SoundDeviceSink {}

    impl SoundDeviceSink {
        /// Opens the output device at index (as output_devices lists them).
        pub fn open(index: usize) -> Result<SoundDeviceSink, String> {
            let device = cpal::default_host().output_devices().map_err(|e| e.to_string())?.nth(index)
                .ok_or_else(|| "the device is gone".to_string())?;
            let queue: Queue = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
            let default = device.default_output_config().map_err(|e| e.to_string())?;
            let channels = default.channels() as usize;
            let config = cpal::StreamConfig { channels: default.channels(), sample_rate: RATE as u32, buffer_size: cpal::BufferSize::Default };
            let q = queue.clone();
            let err = |e: cpal::Error| log::warn!("Microphone output: {e}");
            let stream = match default.sample_format() {
                cpal::SampleFormat::I16 => device.build_output_stream(config, move |out: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    fill(&q, out, channels, |s| s);
                }, err, None),
                _ => device.build_output_stream(config, move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    fill(&q, out, channels, |s| s as f32 / 32768.0);
                }, err, None),
            }
            .map_err(|e| e.to_string())?;
            stream.play().map_err(|e| e.to_string())?;
            Ok(SoundDeviceSink { queue, _stream: stream })
        }
    }

    /// The callback: each mono sample to every channel, silence when nothing is queued.
    fn fill<T: Copy + Default>(q: &Queue, out: &mut [T], channels: usize, convert: impl Fn(i16) -> T) {
        let (lock, cv) = &**q;
        let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
        for frame in out.chunks_mut(channels.max(1)) {
            let v = queue.pop_front().map(&convert).unwrap_or_default();
            frame.iter_mut().for_each(|s| *s = v);
        }
        cv.notify_all();
    }

    impl MicSink for SoundDeviceSink {
        fn write(&mut self, samples: &[i16]) -> io::Result<()> {
            let (lock, cv) = &*self.queue;
            let until = Instant::now() + STALL;
            let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
            while queue.len() > QUEUE {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(()); // nothing plays it: drop the chunk rather than queue up delay
                }
                queue = cv.wait_timeout(queue, left).unwrap_or_else(|e| e.into_inner()).0;
            }
            queue.extend(samples);
            Ok(())
        }
    }
}
