//! Reading Spout senders on Windows: the sender list and each sender's texture through the shared Spout pieces, as packed pixels or as I420 planes made on the card.

use super::frame_count::FrameTracker;
use super::grab::{self, Grabber};
use super::{Backend, Picture, Reader, Sender, I420};
use crate::spout_shared::protocol::ChannelOrder;
use crate::spout_shared::win::{self as shared, AccessLock, Converter, FrameCounter, Gpu, Info, SharedTexture, Staging};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::balance::Balance;

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

/// How many frames in a row the card's conversion may fail before this reader stops asking it.
const CARD_FAILS_ALLOWED: u32 = 3;

/// Whether a failure of the card's conversion has been logged yet; the first is, the rest aren't, so a card that fails every frame can't fill the log.
static CARD_FAILURE_LOGGED: AtomicBool = AtomicBool::new(false);

fn card_failed(key: &str, why: &str) {
    if !CARD_FAILURE_LOGGED.swap(true, Ordering::Relaxed) {
        log::warn!("Spout sender {key}: converting on the graphics card failed ({why}), taking pictures as BGRA instead");
    } else {
        log::debug!("Spout sender {key}: converting on the graphics card failed again: {why}");
    }
}

/// The card's converter for a reader's texture.
enum Card {
    /// Not made yet: the first frame the balance puts on the card makes it.
    Unmade,
    Ready(Converter),
    /// Couldn't be made, or failed too often: this reader takes BGRA from here on.
    Broken,
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
    balance: Arc<Balance>,
    card: Card,
    card_fails: u32,
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
        SpoutReader::with_texture(key, handle, gpu, texture)
    }

    fn with_texture(key: &str, handle: u32, gpu: Gpu, texture: SharedTexture) -> Result<SpoutReader, String> {
        let order = ChannelOrder::of_dxgi(texture.format).ok_or_else(|| format!("the sender's picture format (DXGI {}) isn't supported", texture.format))?;
        let staging = gpu.staging(&texture, 4)?;
        let lock = AccessLock::for_texture(key, &texture);
        log::info!("Spout sender {key}: {}x{} on {}", texture.width, texture.height, gpu.adapter);
        Ok(SpoutReader {
            key: key.into(), handle, gpu, texture, staging, lock, order, counter: FrameCounter::open(key), tracker: FrameTracker::default(), checked: Instant::now(),
            balance: Balance::shared(), card: Card::Unmade, card_fails: 0,
        })
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

    /// The sender's picture now, or None while it's holding the texture, the way the balance has this frame: BGRA or I420 from the card.
    fn grab(&mut self) -> Result<Option<Picture>, String> {
        let balance = self.balance.clone();
        grab::grab(&balance, self)
    }

    /// Takes the picture as BGRA; the count is read while the texture is locked so it belongs to the picture copied, and only a picture that was read is marked as read.
    fn grab_bgra(&mut self) -> Result<Option<Picture>, String> {
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

    /// The converter, made the first time it's wanted; None once it can't be had.
    fn converter(&mut self) -> Option<&Converter> {
        if matches!(self.card, Card::Unmade) {
            self.card = match self.gpu.converter(&self.texture) {
                Ok(converter) => Card::Ready(converter),
                Err(why) => {
                    card_failed(&self.key, &why);
                    Card::Broken
                }
            };
        }
        match &self.card {
            Card::Ready(converter) => Some(converter),
            _ => None,
        }
    }

    /// The BGRA of a picture the card is holding in the converter's copy, for a frame its conversion failed on.
    fn bgra_from_card(&mut self) -> Result<Picture, String> {
        let Card::Ready(converter) = &self.card else { return Err("no converter to take the picture from".into()) };
        self.gpu.copy_source(&self.staging, converter);
        let data = self.gpu.read(&self.staging)?;
        Ok(Picture { width: self.texture.width, height: self.texture.height, fourcc: fourcc(self.order), data })
    }
}

impl Grabber for SpoutReader {
    fn bgra(&mut self) -> Result<Option<Picture>, String> {
        self.grab_bgra()
    }

    fn card(&mut self) -> Result<Option<(Picture, Option<Duration>)>, String> {
        if self.converter().is_none() {
            return Ok(self.grab_bgra()?.map(|picture| (picture, None)));
        }
        if !self.lock.acquire() {
            return Ok(None);
        }
        let count = self.count();
        // The lock only covers the copy into the converter's own texture; the shaders run after it's let go
        let copied = match &self.card {
            Card::Ready(converter) => self.gpu.copy_to_converter(converter, &self.texture),
            _ => Err("no converter".into()),
        };
        self.lock.release();
        if let Err(why) = copied {
            card_failed(&self.key, &why);
            self.card = Card::Broken;
            return Ok(self.grab_bgra()?.map(|picture| (picture, None)));
        }
        let started = Instant::now();
        let made = match &self.card {
            Card::Ready(converter) => self.gpu.convert(converter),
            _ => Err("no converter".into()),
        };
        let wait = started.elapsed();
        let (picture, wait) = match made {
            Ok(data) => {
                self.card_fails = 0;
                (Picture { width: self.texture.width, height: self.texture.height, fourcc: I420, data }, Some(wait))
            }
            Err(why) => {
                card_failed(&self.key, &why);
                self.card_fails += 1;
                let picture = self.bgra_from_card();
                if self.card_fails >= CARD_FAILS_ALLOWED {
                    self.card = Card::Broken;
                }
                (picture?, None)
            }
        };
        self.tracker.read(count);
        Ok(Some((picture, wait)))
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
    use crate::convert::bgra_to_i420;
    use crate::spout_shared::win::testing::{noise, texture_with, warp};
    use telescope_core::balance::Place;
    use telescope_core::media::{Decoder, Packet};

    /// A reader on a WARP texture of random pixels, with the balance fixed at `place`.
    fn reader_at(place: Place, size: (u32, u32)) -> (SpoutReader, Vec<u8>, Arc<Balance>) {
        let gpu = warp();
        let data = noise(size.0 as usize * size.1 as usize * 4, size.0 * 31 + size.1);
        let texture = texture_with(&gpu, size, 87, &data);
        let mut reader = SpoutReader::with_texture("telescope-test-sender", 0, gpu, texture).unwrap();
        let balance = Arc::new(Balance::pinned_to(place));
        reader.balance = balance.clone();
        (reader, data, balance)
    }

    fn within_one(picture: &Picture, data: &[u8]) {
        let want = bgra_to_i420(data, picture.width, picture.height).data.to_vec();
        assert_eq!(picture.data.len(), want.len());
        assert!(picture.data.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= 1));
    }

    #[test]
    fn with_the_balance_on_the_cpu_the_picture_is_the_sender_s_bgra_bytes() {
        let (mut reader, data, balance) = reader_at(Place::Cpu, (17, 9));
        let picture = reader.grab().unwrap().unwrap();
        assert_eq!((picture.fourcc, picture.width, picture.height), ("BGRA", 17, 9));
        assert_eq!(picture.data, data);
        assert_eq!(balance.last_wait(), 0);
        assert!(matches!(reader.card, Card::Unmade));
    }

    #[test]
    fn with_the_balance_on_the_card_the_picture_is_planes_close_to_the_cpu_s_and_the_wait_is_told() {
        for size in [(17, 9), (1, 1), (64, 48), (1919, 1079)] {
            let (mut reader, data, balance) = reader_at(Place::Card, size);
            let picture = reader.grab().unwrap().unwrap();
            assert_eq!((picture.fourcc, picture.width, picture.height), (I420, size.0, size.1));
            within_one(&picture, &data);
            assert!(balance.last_wait() > 0);
            // And the stream's decoder takes it as a frame
            let packet = Packet { data: super::super::packet_of(&picture), time_ns: 0 };
            let frame = super::super::SpoutDecoder.decode(&packet).newest.unwrap();
            assert_eq!(frame.data.to_vec(), picture.data);
        }
    }

    #[test]
    fn a_frame_the_shaders_fail_on_is_the_same_picture_as_bgra_and_the_next_is_on_the_card_again() {
        let (mut reader, data, balance) = reader_at(Place::Card, (17, 9));
        reader.grab().unwrap().unwrap();
        let wait = balance.last_wait();
        let Card::Ready(converter) = &reader.card else { panic!("no converter") };
        converter.fail_next.store(true, Ordering::SeqCst);
        let fell_back = reader.grab().unwrap().unwrap();
        assert_eq!((fell_back.fourcc, &fell_back.data), ("BGRA", &data));
        assert_eq!(balance.last_wait(), wait, "a frame that fell back has no wait to tell");
        assert_eq!(reader.grab().unwrap().unwrap().fourcc, I420);
    }

    #[test]
    fn a_card_that_keeps_failing_is_given_up_on_and_pictures_stay_bgra() {
        let (mut reader, data, _) = reader_at(Place::Card, (5, 3));
        reader.grab().unwrap().unwrap();
        for _ in 0..CARD_FAILS_ALLOWED {
            let Card::Ready(converter) = &reader.card else { panic!("gave up too soon") };
            converter.fail_next.store(true, Ordering::SeqCst);
            assert_eq!(reader.grab().unwrap().unwrap().data, data);
        }
        assert!(matches!(reader.card, Card::Broken));
        let picture = reader.grab().unwrap().unwrap();
        assert_eq!((picture.fourcc, picture.data), ("BGRA", data));
    }

    #[test]
    fn a_failure_now_and_then_doesnt_add_up_to_giving_up() {
        let (mut reader, _, _) = reader_at(Place::Card, (5, 3));
        for _ in 0..2 * CARD_FAILS_ALLOWED {
            reader.grab().unwrap().unwrap();
            let Card::Ready(converter) = &reader.card else { panic!("gave up") };
            converter.fail_next.store(true, Ordering::SeqCst);
            reader.grab().unwrap().unwrap();
        }
        assert!(matches!(reader.card, Card::Ready(_)));
    }

    /// 1080p frames from a texture on this device, taken the CPU's way and the card's: grab, wrap as a packet and decode, as the stream does.
    #[test]
    #[ignore = "timing: cargo test --release -p telescope-outputs card_against_cpu -- --ignored --nocapture"]
    fn card_against_cpu_conversion_time_at_1080p() {
        for place in [Place::Cpu, Place::Card] {
            let (mut reader, _, _) = reader_at(place, (1920, 1080));
            let decoder = super::super::SpoutDecoder;
            for _ in 0..10 {
                reader.grab().unwrap().unwrap();
            }
            let (frames, mut grab_total, mut decode_total) = (100u32, Duration::ZERO, Duration::ZERO);
            for _ in 0..frames {
                let started = Instant::now();
                let picture = reader.grab().unwrap().unwrap();
                grab_total += started.elapsed();
                let started = Instant::now();
                let packet = super::super::packet_of(&picture);
                std::hint::black_box(decoder.frame(&packet).unwrap());
                decode_total += started.elapsed();
            }
            let ms = |d: Duration| d.as_secs_f64() * 1000.0 / frames as f64;
            println!("{place:?} on {}: grab {:.2} ms, packet and decode {:.2} ms, together {:.2} ms a frame", reader.gpu.adapter, ms(grab_total), ms(decode_total), ms(grab_total + decode_total));
        }
    }

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
