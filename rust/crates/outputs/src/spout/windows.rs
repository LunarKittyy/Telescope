//! Reading Spout senders on Windows: the sender list and each sender's texture through the shared Spout pieces, copied out as packed pixels.

use super::frame_count::FrameTracker;
use super::{Backend, Picture, Reader, Sender};
use crate::spout_shared::protocol::ChannelOrder;
use crate::spout_shared::win::{self as shared, AccessLock, FrameCounter, Gpu, Info, SharedTexture, Staging};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How often a reader looks at the sender while it waits for a new picture.
const POLL: Duration = Duration::from_millis(2);
/// How often a reader checks the sender is still on the list, and looks for a frame counter it hasn't found.
const CHECK: Duration = Duration::from_millis(500);

pub fn backend() -> Option<Box<dyn Backend>> {
    Some(Box::new(Senders { last: Mutex::new(Vec::new()) }))
}

struct Senders {
    /// The list as it was last read, for a moment the sender list is busy.
    last: Mutex<Vec<Sender>>,
}

impl Backend for Senders {
    fn senders(&self) -> Vec<Sender> {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let Some(names) = shared::sender_names() else { return last.clone() };
        *last = names
            .into_iter()
            .filter_map(|name| match shared::sender_info(&name) {
                // A name nobody publishes any more (its sender crashed) is skipped
                Info::Found(info) => Some(Sender { key: name.clone(), name, width: info.width, height: info.height }),
                Info::Busy => last.iter().find(|s| s.key == name).cloned(),
                Info::Gone => None,
            })
            .collect();
        last.clone()
    }

    fn open(&self, key: &str) -> Result<Box<dyn Reader>, String> {
        let Info::Found(info) = shared::sender_info(key) else { return Err("the sender isn't running".into()) };
        Ok(Box::new(SpoutReader::open(key, info.share_handle)?))
    }
}

struct SpoutReader {
    key: String,
    handle: u32,
    gpu: Gpu,
    texture: SharedTexture,
    staging: Staging,
    lock: AccessLock,
    order: ChannelOrder,
    counter: Option<FrameCounter>,
    tracker: FrameTracker,
    checked: Instant,
}

fn fourcc(order: ChannelOrder) -> &'static str {
    match order {
        ChannelOrder::Bgra => "BGRA",
        ChannelOrder::Bgrx => "BGRx",
        ChannelOrder::Rgba => "RGBA",
    }
}

impl SpoutReader {
    fn open(key: &str, handle: u32) -> Result<SpoutReader, String> {
        let (gpu, texture) = shared::open_shared_texture(handle)?;
        let order = ChannelOrder::of_dxgi(texture.format).ok_or_else(|| format!("the sender's picture format (DXGI {}) isn't supported", texture.format))?;
        let staging = gpu.staging(&texture, 4)?;
        let lock = AccessLock::for_texture(key, &texture);
        log::info!("Spout sender {key}: {}x{} on {}", texture.width, texture.height, gpu.adapter);
        Ok(SpoutReader { key: key.into(), handle, gpu, texture, staging, lock, order, counter: FrameCounter::open(key), tracker: FrameTracker::default(), checked: Instant::now() })
    }

    /// The sender's frame count now, None for one that doesn't count.
    fn count(&self) -> Option<i32> {
        self.counter.as_ref().and_then(FrameCounter::read)
    }

    /// Follows the sender to a new texture when it restarted or changed size.
    fn follow(&mut self) -> Result<(), String> {
        match shared::sender_info(&self.key) {
            Info::Gone => return Err("the sender has stopped".into()),
            Info::Busy => return Ok(()),
            Info::Found(info) => {
                if info.share_handle != self.handle || info.width != self.texture.width || info.height != self.texture.height {
                    *self = SpoutReader::open(&self.key, info.share_handle)?;
                }
            }
        }
        if self.checked.elapsed() >= CHECK {
            self.checked = Instant::now();
            if shared::sender_names().is_some_and(|names| !names.contains(&self.key)) {
                return Err("the sender has stopped".into());
            }
            if self.counter.is_none() {
                self.counter = FrameCounter::open(&self.key);
            }
        }
        Ok(())
    }

    /// The sender's picture now, or None while it's holding the texture. The count is read while the texture is locked, so it belongs to the
    /// picture copied, and only a picture that was read is marked as read.
    fn grab(&mut self) -> Result<Option<Picture>, String> {
        if !self.lock.acquire() {
            return Ok(None);
        }
        let count = self.count();
        self.gpu.copy(&self.staging, &self.texture);
        self.lock.release();
        let data = self.gpu.read(&self.staging)?;
        self.tracker.read(count);
        Ok(Some(Picture { width: self.texture.width, height: self.texture.height, fourcc: fourcc(self.order), data }))
    }
}

impl Reader for SpoutReader {
    fn next(&mut self, timeout: Duration) -> Result<Option<Picture>, String> {
        let end = Instant::now() + timeout;
        loop {
            self.follow()?;
            if self.tracker.may_be_new(self.count()) {
                return self.grab();
            }
            if Instant::now() >= end {
                return Ok(None);
            }
            std::thread::sleep(POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a Spout sender running. SPOUT_TEST_SENDER picks one by name (else the first), SPOUT_TEST_PNG says where its first picture is saved.
    #[test]
    #[ignore = "needs a running Spout sender"]
    fn a_running_sender_is_listed_and_gives_a_picture() {
        let backend = backend().unwrap();
        let senders = backend.senders();
        println!("senders: {senders:?}");
        let want = std::env::var("SPOUT_TEST_SENDER").ok();
        let sender = senders.iter().find(|s| want.as_ref().is_none_or(|w| &s.name == w)).expect("no Spout sender is running");
        let mut reader = backend.open(&sender.key).unwrap();
        let mut first = None;
        for _ in 0..10 {
            if let Some(picture) = reader.next(Duration::from_secs(1)).unwrap() {
                first = Some(picture);
                break;
            }
        }
        let picture = first.expect("the sender gave no picture in 10 s");
        assert_eq!((picture.width, picture.height), (sender.width, sender.height));
        assert_eq!(picture.data.len(), picture.width as usize * picture.height as usize * 4);
        // Per channel means of the colour bytes, in RGB order
        let order = if picture.fourcc.starts_with("BGR") { [2, 1, 0] } else { [0, 1, 2] };
        let pixels = picture.data.len() as u64 / 4;
        let means: Vec<u64> = order.iter().map(|c| picture.data.as_chunks::<4>().0.iter().map(|p| p[*c] as u64).sum::<u64>() / pixels).collect();
        println!("picture {}x{} {} mean RGB {means:?}", picture.width, picture.height, picture.fourcc);
        // A second picture comes at the sender's pace, not at once (frame counting) or at least still arrives
        let started = Instant::now();
        let mut more = 0;
        while started.elapsed() < Duration::from_secs(2) {
            if reader.next(Duration::from_millis(500)).unwrap().is_some() {
                more += 1;
            }
        }
        println!("{more} more pictures in 2 s");
        if let Ok(path) = std::env::var("SPOUT_TEST_PNG") {
            let rgba: Vec<u8> = picture.data.as_chunks::<4>().0.iter().flat_map(|p| [p[order[0]], p[order[1]], p[order[2]], 255]).collect();
            image::save_buffer(path, &rgba, picture.width, picture.height, image::ColorType::Rgba8).unwrap();
        }
    }

    /// Needs a Spout sender that is closed while this runs: the reader ends with an error and the sender leaves the list.
    #[test]
    #[ignore = "needs a Spout sender that gets closed during the test"]
    fn a_sender_that_closes_ends_the_reader_and_leaves_the_list() {
        let backend = backend().unwrap();
        let sender = backend.senders().into_iter().next().expect("no Spout sender is running");
        let mut reader = backend.open(&sender.key).unwrap();
        let started = Instant::now();
        let why = loop {
            assert!(started.elapsed() < Duration::from_secs(30), "the reader never ended");
            if let Err(why) = reader.next(Duration::from_millis(200)) {
                break why;
            }
        };
        println!("reader ended after {:?}: {why}", started.elapsed());
        let listed = || backend.senders().iter().any(|s| s.key == sender.key);
        let waited = Instant::now();
        while listed() && waited.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(200));
        }
        println!("still listed after {:?}: {}", waited.elapsed(), listed());
        assert!(!listed());
    }
}
