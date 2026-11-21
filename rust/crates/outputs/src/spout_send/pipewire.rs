//! Spout send on Linux: a PipeWire video source for each shared stream.

use super::{Backend, Resized, Sender};
use crate::pipewire::{Connection, VideoSender};
use std::sync::{Arc, Mutex};

pub fn backend() -> Option<Arc<dyn Backend>> {
    Some(Arc::new(PipeWireBackend { connection: Mutex::new(None) }))
}

struct PipeWireBackend {
    connection: Mutex<Option<Arc<Connection>>>,
}

impl PipeWireBackend {
    /// The connection, made again if PipeWire was restarted since.
    fn connection(&self) -> Result<Arc<Connection>, String> {
        let mut held = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(connection) = held.clone().filter(|c| !c.is_dead()) {
            return Ok(connection);
        }
        let connection = Connection::shared()?;
        *held = Some(connection.clone());
        Ok(connection)
    }
}

impl Backend for PipeWireBackend {
    // A program that bound to the node by its id (OBS) would lose it with the node
    fn keeps_idle_senders(&self) -> bool {
        true
    }

    fn open(&self, name: &str, width: u32, height: u32) -> Result<Box<dyn Sender>, String> {
        let connection = self.connection()?;
        Ok(Box::new(VideoSender::open(&connection, name, width, height)?))
    }
}

impl Sender for VideoSender {
    fn wants_frame(&self) -> bool {
        VideoSender::wants_frame(self)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<Resized, String> {
        VideoSender::resize(self, width, height);
        Ok(Resized::Yes)
    }

    fn send(&mut self, bgra: &[u8]) -> Result<(), String> {
        VideoSender::send(self, bgra)
    }

    fn send_buffer(&mut self, bgra: &mut Vec<u8>) -> Result<(), String> {
        VideoSender::send_buffer(self, bgra)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipewire::VideoFormat;
    use std::time::{Duration, Instant};

    fn picture(w: u32, h: u32) -> Vec<u8> {
        (0..w * h).flat_map(|i| [(i % 251) as u8, 40, 200, 255]).collect()
    }

    #[test]
    fn a_picture_sent_comes_out_of_the_pipewire_source_as_it_went_in() {
        let Ok(connection) = Connection::shared() else { return eprintln!("skipped: PipeWire isn't running") };
        let backend = backend().unwrap();
        let name = format!("Telescope send test {}", std::process::id());
        let (w, h) = (64, 36);
        let mut sender = backend.open(&name, w, h).expect("the source should open");
        let until = Instant::now() + Duration::from_secs(8);
        let node = loop {
            if let Some(node) = connection.nodes().into_iter().find(|n| n.props.iter().any(|(k, v)| k == "node.description" && *v == name)) {
                break node;
            }
            assert!(Instant::now() < until, "the source never showed in PipeWire");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut consumer = crate::pipewire::VideoConsumer::open(&connection, node.id, "Telescope send test reader").expect("the source should open for reading");
        let sent = picture(w, h);
        let until = Instant::now() + Duration::from_secs(8);
        let got = loop {
            sender.send(&sent).unwrap();
            if let Some(frame) = consumer.next(Duration::from_millis(100)).expect("reading shouldn't fail") {
                break frame;
            }
            assert!(Instant::now() < until, "no picture arrived");
        };
        assert_eq!((got.width, got.height, got.format), (w, h, VideoFormat::Bgra));
        assert_eq!(got.data, sent);
    }

    #[test]
    fn a_buffer_handed_over_comes_out_as_it_went_in_and_its_replacement_is_reusable() {
        let Ok(connection) = Connection::shared() else { return eprintln!("skipped: PipeWire isn't running") };
        let name = format!("Telescope send buffer test {}", std::process::id());
        let (w, h) = (64, 36);
        let mut sender = backend().unwrap().open(&name, w, h).expect("the source should open");
        let until = Instant::now() + Duration::from_secs(8);
        let node = loop {
            if let Some(node) = connection.nodes().into_iter().find(|n| n.props.iter().any(|(k, v)| k == "node.description" && *v == name)) {
                break node;
            }
            assert!(Instant::now() < until, "the source never showed in PipeWire");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut consumer = crate::pipewire::VideoConsumer::open(&connection, node.id, "Telescope send buffer reader").expect("the source should open for reading");
        let sent = picture(w, h);
        let mut buffer = Vec::new();
        let until = Instant::now() + Duration::from_secs(8);
        let got = loop {
            buffer.clear();
            buffer.extend_from_slice(&sent);
            sender.send_buffer(&mut buffer).unwrap();
            if let Some(frame) = consumer.next(Duration::from_millis(100)).expect("reading shouldn't fail") {
                break frame;
            }
            assert!(Instant::now() < until, "no picture arrived");
        };
        assert_eq!(got.data, sent);
        // A wrong size is still refused
        assert!(sender.send_buffer(&mut vec![0; 10]).is_err());
    }

    #[test]
    fn a_source_changes_size_where_it_is_and_the_reader_follows() {
        let Ok(connection) = Connection::shared() else { return eprintln!("skipped: PipeWire isn't running") };
        let name = format!("Telescope send resize test {}", std::process::id());
        let mut sender = backend().unwrap().open(&name, 64, 36).expect("the source should open");
        let until = Instant::now() + Duration::from_secs(8);
        let node = loop {
            if let Some(node) = connection.nodes().into_iter().find(|n| n.props.iter().any(|(k, v)| k == "node.description" && *v == name)) {
                break node;
            }
            assert!(Instant::now() < until, "the source never showed in PipeWire");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut consumer = crate::pipewire::VideoConsumer::open(&connection, node.id, "Telescope send resize reader").expect("the source should open for reading");
        let first = picture(64, 36);
        let until = Instant::now() + Duration::from_secs(8);
        while consumer.next(Duration::from_millis(100)).unwrap().is_none() {
            sender.send(&first).unwrap();
            assert!(Instant::now() < until, "no picture arrived");
        }
        assert_eq!(sender.resize(32, 18), Ok(Resized::Yes));
        let second = picture(32, 18);
        let until = Instant::now() + Duration::from_secs(8);
        let got = loop {
            sender.send(&second).unwrap();
            match consumer.next(Duration::from_millis(100)) {
                Ok(Some(frame)) if frame.width == 32 => break frame,
                Ok(_) => {}
                Err(why) => panic!("the reader gave up: {why}"),
            }
            assert!(Instant::now() < until, "the reader never followed the new size");
        };
        assert_eq!((got.width, got.height), (32, 18));
        assert!(connection.node(node.id).is_some(), "the node stays");
    }

    #[test]
    fn a_picture_of_the_wrong_size_is_refused() {
        let Ok(_) = Connection::shared() else { return eprintln!("skipped: PipeWire isn't running") };
        let name = format!("Telescope send size test {}", std::process::id());
        let mut sender = backend().unwrap().open(&name, 8, 8).expect("the source should open");
        assert!(sender.send(&[0; 10]).is_err());
    }

    #[test]
    fn zz_trailing_sends_then_drop() {
        let Ok(_) = Connection::shared() else { return eprintln!("skipped: PipeWire isn't running") };
        let backend = backend().unwrap();
        let pic = picture(64, 36);
        for round in 0..40 {
            let name = format!("Telescope send drop test {} {round}", std::process::id());
            let mut sender = backend.open(&name, 64, 36).expect("the source should open");
            for _ in 0..3 {
                sender.send(&pic).unwrap();
            }
            drop(sender);
        }
    }
}
