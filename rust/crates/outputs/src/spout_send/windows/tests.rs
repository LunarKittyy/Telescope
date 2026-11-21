//! A sender made through the backend, read back through the reading half of the Spout pieces.

use super::*;
use crate::spout_shared::poll::Deadline;
use crate::spout_shared::win::testing::noise;
use crate::spout_shared::protocol::{parse_names, ACTIVE_MAP, NAME_LEN};
use crate::spout_shared::win::sender::{HeldAccess, StaleList};
use crate::spout_shared::win::{open_shared_texture, sender_info, sender_names, FrameCounter, Info, SharedMemory};
use std::sync::Mutex;
use std::time::Duration;

/// The sender list is one for the whole machine and holds ten, so the tests take turns on it.
static LIST: Mutex<()> = Mutex::new(());

fn turn() -> std::sync::MutexGuard<'static, ()> {
    LIST.lock().unwrap_or_else(|e| e.into_inner())
}

fn backend() -> Arc<dyn Backend> {
    super::backend().expect("a Spout backend on Windows")
}

/// The sender's texture as a receiver reads it: size, format and the pixels, packed.
fn read_back(name: &str) -> (u32, u32, u32, Vec<u8>) {
    let Info::Found(info) = sender_info(name) else { panic!("{name} publishes no info") };
    let (gpu, texture) = open_shared_texture(info.share_handle).expect("opening the shared texture");
    let staging = gpu.staging(&texture, 4).expect("a staging texture");
    gpu.copy(&staging, &texture);
    let pixels = gpu.read(&staging, &Deadline::within(Duration::from_secs(10))).expect("reading the pixels");
    (texture.width, texture.height, texture.format, pixels)
}

#[test]
fn a_sender_is_listed_with_its_size_and_format_and_gone_after_it_is_dropped() {
    let _turn = turn();
    let name = "Telescope test listed";
    let sender = backend().open(name, 64, 32).expect("a sender");
    assert!(sender_names().expect("the list").contains(&name.to_string()));
    let Info::Found(info) = sender_info(name) else { panic!("no info") };
    assert_eq!((info.width, info.height, info.format, info.usage, info.partner_id), (64, 32, 87, 0, 0));
    assert!(info.description[0] != 0);
    drop(sender);
    assert!(!sender_names().expect("the list").contains(&name.to_string()));
    assert_eq!(sender_info(name), Info::Gone);
}

#[test]
fn a_picture_sent_is_what_a_receiver_reads() {
    let _turn = turn();
    let name = "Telescope test pixels";
    let mut sender = backend().open(name, 64, 32).expect("a sender");
    let mut picture = noise(64 * 32 * 4, 7);
    for pixel in picture.chunks_mut(4) {
        pixel[3] = 255;
    }
    sender.send(&picture).expect("sending");
    let (width, height, format, pixels) = read_back(name);
    assert_eq!((width, height, format), (64, 32, 87));
    assert_eq!(pixels, picture);
    // A second picture replaces the first
    let second: Vec<u8> = picture.iter().map(|b| !b).collect();
    sender.send(&second).expect("sending again");
    assert_eq!(read_back(name).3, second);
}

#[test]
fn every_frame_sent_is_counted_for_receivers() {
    let _turn = turn();
    let name = "Telescope test counting";
    let mut sender = backend().open(name, 16, 16).expect("a sender");
    let counter = FrameCounter::open(name).expect("a frame counter");
    assert_eq!(counter.read(), Some(0));
    for n in 1..=3 {
        sender.send(&[200u8; 16 * 16 * 4]).expect("sending");
        assert_eq!(counter.read(), Some(n));
    }
}

#[test]
fn a_name_in_use_or_a_picture_of_the_wrong_size_is_refused() {
    let _turn = turn();
    let name = "Telescope test twice";
    let mut first = backend().open(name, 16, 16).expect("a sender");
    assert!(backend().open(name, 16, 16).is_err());
    assert!(sender_names().expect("the list").contains(&name.to_string()));
    assert!(first.send(&[0u8; 10]).is_err());
    drop(first);
    // The name can be had again once the first is gone
    assert!(backend().open(name, 16, 16).is_ok());
}

#[test]
fn the_list_refuses_an_eleventh_sender_and_closes_the_gap_when_one_leaves() {
    let _turn = turn();
    let mut senders: Vec<Box<dyn Sender>> = Vec::new();
    let names: Vec<String> = (0..10).map(|i| format!("Telescope test full {i}")).collect();
    let mut opened = 0;
    for name in &names {
        match backend().open(name, 8, 8) {
            Ok(sender) => {
                senders.push(sender);
                opened += 1;
            }
            Err(why) => {
                // Other senders on the machine may already hold some of the ten places
                assert!(why.contains("full"), "{why}");
                break;
            }
        }
    }
    if opened == 10 {
        assert!(backend().open("Telescope test full extra", 8, 8).err().expect("a full list").contains("full"));
    }
    senders.remove(0);
    let listed = sender_names().expect("the list");
    assert!(!listed.contains(&names[0]));
    if opened > 1 {
        assert!(listed.contains(&names[1]));
    }
}

/// The name receivers take as the active sender, empty when there is none.
fn active_name() -> String {
    let Some(memory) = SharedMemory::open(ACTIVE_MAP) else { return String::new() };
    parse_names(&memory.read(NAME_LEN).expect("the active name")).into_iter().next().unwrap_or_default()
}

#[test]
fn a_name_left_in_the_list_by_a_program_that_ended_is_taken_over_once() {
    let _turn = turn();
    let name = "Telescope test stale";
    let _stale = StaleList::with(name);
    assert!(sender_names().expect("the list").contains(&name.to_string()));
    let sender = backend().open(name, 16, 16).expect("a sender over the stale name");
    assert_eq!(sender_names().expect("the list").iter().filter(|n| *n == name).count(), 1);
    drop(sender);
    assert!(!sender_names().expect("the list").contains(&name.to_string()));
}

#[test]
fn a_sender_resized_keeps_its_name_and_counter_and_shows_the_new_picture() {
    let _turn = turn();
    let name = "Telescope test resize";
    let mut sender = backend().open(name, 16, 8).expect("a sender");
    let counter = FrameCounter::open(name).expect("a frame counter");
    sender.send(&[10u8; 16 * 8 * 4]).expect("sending");
    // A receiver holding the info mapping, as OBS does, must not stop the resize
    let held = SharedMemory::open(name).expect("the info mapping");
    assert_eq!(sender.resize(32, 24), Ok(Resized::Yes));
    assert_eq!(sender.resize(32, 24), Ok(Resized::Yes));
    let mut picture = noise(32 * 24 * 4, 3);
    for pixel in picture.chunks_mut(4) {
        pixel[3] = 255;
    }
    sender.send(&picture).expect("sending after the resize");
    let (width, height, format, pixels) = read_back(name);
    assert_eq!((width, height, format), (32, 24, 87));
    assert_eq!(pixels, picture);
    assert_eq!(counter.read(), Some(2));
    assert_eq!(sender_names().expect("the list").iter().filter(|n| *n == name).count(), 1);
    assert!(sender.send(&[0u8; 16 * 8 * 4]).is_err());
    drop(held);
}

#[test]
fn the_first_sender_is_the_active_one_and_leaves_no_name_behind() {
    let _turn = turn();
    let (first, second) = ("Telescope test active 1", "Telescope test active 2");
    let one = backend().open(first, 8, 8).expect("a sender");
    assert_eq!(active_name(), first);
    let two = backend().open(second, 8, 8).expect("another sender");
    assert_eq!(active_name(), first);
    drop(two);
    assert_eq!(active_name(), first);
    drop(one);
    assert_eq!(active_name(), "");
}

#[test]
fn a_receiver_holding_the_picture_while_the_size_changes_leaves_the_sender_listed_and_the_resize_for_later() {
    let _turn = turn();
    let name = "Telescope test resize busy";
    let mut sender = backend().open(name, 16, 8).expect("a sender");
    sender.send(&[10u8; 16 * 8 * 4]).expect("sending");
    let reading = HeldAccess::of(name);
    assert_eq!(sender.resize(32, 24), Ok(Resized::Busy));
    assert!(sender_names().expect("the list").contains(&name.to_string()), "the sender went from the list");
    let Info::Found(info) = sender_info(name) else { panic!("no info") };
    assert_eq!((info.width, info.height), (16, 8), "the info is as it was");
    drop(reading);
    assert_eq!(sender.resize(32, 24), Ok(Resized::Yes));
    let Info::Found(info) = sender_info(name) else { panic!("no info") };
    assert_eq!((info.width, info.height), (32, 24));
}
