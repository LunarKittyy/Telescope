//! Spout senders on Linux: Spout2PW bridges each sender running under Proton to a PipeWire video node, so this lists
//! those nodes (and no others) as senders and reads them through the shared PipeWire consumer.

use super::{Backend, Picture, Reader, Sender};
use crate::pipewire::{Connection, Node, VideoConsumer, VideoFrame};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the stream is called in PipeWire's graph tools.
const CLIENT_NAME: &str = "Telescope Spout capture";
/// Spout2PW names its nodes "<app id>.<sender>", with its app id "spout2pw" (or "spout2pw.<name>" when SPOUT2PW_APPNAME is set).
const NODE_PREFIX: &str = "spout2pw";

/// The PipeWire side of Spout capture, or None when PipeWire can't be reached.
pub fn backend() -> Option<Box<dyn Backend>> {
    let connection = Connection::shared().ok()?;
    Some(Box::new(PipeWireBackend { connection: Mutex::new(connection) }))
}

struct PipeWireBackend {
    connection: Mutex<Arc<Connection>>,
}

impl PipeWireBackend {
    /// The connection, made again if PipeWire was restarted since.
    fn connection(&self) -> Option<Arc<Connection>> {
        let mut held = self.connection.lock().ok()?;
        if held.is_dead() {
            *held = Connection::shared().ok()?;
        }
        Some(held.clone())
    }
}

/// Whether a node is one Spout2PW made for a sender: a video output stream with its name or app id.
fn is_spout2pw(node: &Node) -> bool {
    let from_spout2pw = |text: &str| text == NODE_PREFIX || text.strip_prefix(NODE_PREFIX).is_some_and(|rest| rest.starts_with('.'));
    node.media_class() == "Stream/Output/Video" && (from_spout2pw(node.name()) || node.prop("application.id").is_some_and(from_spout2pw))
}

/// The Spout sender's own name: the node's nick, or what follows the app name in its description.
fn sender_name(node: &Node) -> String {
    if let Some(nick) = node.prop("node.nick").filter(|n| !n.is_empty()) {
        return nick.to_string();
    }
    if let Some((_, name)) = node.prop("node.description").and_then(|d| d.split_once(": ")) {
        return name.to_string();
    }
    node.name().strip_prefix(NODE_PREFIX).map(|n| n.trim_start_matches('.')).unwrap_or(node.name()).to_string()
}

impl Backend for PipeWireBackend {
    fn senders(&self) -> Vec<Sender> {
        let Some(connection) = self.connection() else { return Vec::new() };
        let mut senders: Vec<Sender> = Vec::new();
        for node in connection.nodes().iter().filter(|n| is_spout2pw(n)) {
            let key = node.name().to_string();
            if senders.iter().any(|s| s.key == key) {
                continue;
            }
            let (width, height) = node.video_size.unwrap_or((0, 0));
            senders.push(Sender { key, name: sender_name(node), width, height });
        }
        senders
    }

    fn open(&self, key: &str) -> Result<Box<dyn Reader>, String> {
        let connection = self.connection().ok_or("PipeWire isn't running")?;
        let node = connection.nodes().into_iter().find(|n| is_spout2pw(n) && n.name() == key).ok_or_else(|| "the sender isn't running".to_string())?;
        let consumer = VideoConsumer::open(&connection, node.id, CLIENT_NAME)?;
        Ok(Box::new(PipeWireReader { consumer }))
    }
}

struct PipeWireReader {
    consumer: VideoConsumer,
}

impl Reader for PipeWireReader {
    fn next(&mut self, timeout: Duration) -> Result<Option<Picture>, String> {
        Ok(self.consumer.next(timeout)?.map(|VideoFrame { width, height, format, data }| Picture { width, height, fourcc: format.fourcc(), data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipewire::test_source::{test_pixel, FakeSource, Memory, SourceSpec};
    use crate::pipewire::VideoFormat;
    use std::time::Instant;

    /// What libfunnel gives a Spout2PW node for the sender called `sender` (checked against its source).
    fn spout2pw_node(sender: &str, memory: Memory, format: VideoFormat) -> SourceSpec {
        let sanitized: String = sender.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
        let props = [
            ("media.type", "Video".to_string()),
            ("media.class", "Stream/Output/Video".to_string()),
            ("media.role", "Production".to_string()),
            ("node.name", format!("spout2pw.{sanitized}")),
            ("node.description", format!("Spout2PW: {sender}")),
            ("node.nick", sender.to_string()),
            ("media.name", sender.to_string()),
            ("application.name", "Spout2PW".to_string()),
            ("application.id", "spout2pw".to_string()),
        ];
        SourceSpec { name: sender.to_string(), props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(), width: 320, height: 180, format, memory }
    }

    fn wait_for<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(found) = check() {
                return Some(found);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Runs a fake Spout2PW sender through the backend and checks the listing, the colours and the sender going away. Skips with a note when this machine can't.
    fn round_trip(memory: Memory, format: VideoFormat) {
        let Some(backend) = backend() else { return eprintln!("skipped: PipeWire isn't running") };
        let connection = Connection::shared().unwrap();
        let sender = format!("Telescope test {} {:?} {:?}", std::process::id(), memory, format);
        let source = match FakeSource::start(&connection, spout2pw_node(&sender, memory, format)) {
            Ok(source) => source,
            Err(why) => return eprintln!("skipped: {why}"),
        };
        // Something else that is a video output stream but not Spout2PW's stays out of the list
        let mut other = spout2pw_node("Webcam thing", memory, format);
        other.props.retain(|(k, _)| k != "application.id");
        for (k, v) in &mut other.props {
            if k == "node.name" {
                *v = "someapp.webcam".into();
            }
        }
        let _other = FakeSource::start(&connection, other).unwrap();

        let listed = wait_for(Duration::from_secs(5), || backend.senders().into_iter().find(|s| s.name == sender && s.width != 0));
        let listed = listed.expect("the sender should be listed with its size");
        assert_eq!((listed.width, listed.height), (320, 180));
        assert_eq!(listed.key, format!("spout2pw.{}", sender.replace(' ', "_")));
        assert!(backend.senders().iter().all(|s| s.name != "Webcam thing"), "only Spout2PW's nodes are senders");

        let mut reader = backend.open(&listed.key).expect("the sender should open");
        let picture = wait_for(Duration::from_secs(8), || reader.next(Duration::from_millis(500)).expect("reading shouldn't fail")).expect("a picture should arrive");
        assert_eq!((picture.width, picture.height, picture.fourcc), (320, 180, format.fourcc()));
        assert_eq!(picture.data.len(), 320 * 180 * 4);
        for (x, y) in [(0, 0), (319, 0), (159, 100), (160, 100), (0, 179), (319, 179), (10, 140)] {
            let at = (y * 320 + x) as usize * 4;
            let expected = test_pixel(format, 320, 180, x, y);
            // The X formats' fourth byte is whatever the producer left there
            let compared = if matches!(format, VideoFormat::Bgrx | VideoFormat::Rgbx) { 3 } else { 4 };
            assert_eq!(&picture.data[at..at + compared], &expected[..compared], "pixel ({x}, {y})");
        }

        drop(source);
        let gone = wait_for(Duration::from_secs(10), || reader.next(Duration::from_millis(300)).err());
        assert!(gone.is_some(), "the reader should report the sender gone");
        assert!(backend.senders().iter().all(|s| s.name != sender));
    }

    #[test]
    fn reads_a_dma_buf_sender_like_spout2pws() {
        round_trip(Memory::DmaBuf, VideoFormat::Bgra);
    }

    #[test]
    fn reads_a_dma_buf_sender_in_rgba() {
        round_trip(Memory::DmaBuf, VideoFormat::Rgba);
    }

    #[test]
    fn reads_a_memory_sender() {
        round_trip(Memory::MemFd, VideoFormat::Bgrx);
    }

    #[test]
    fn spout2pw_nodes_are_told_from_others() {
        let node = |pairs: &[(&str, &str)]| Node { id: 1, serial: 1, props: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), video_size: None };
        assert!(is_spout2pw(&node(&[("media.class", "Stream/Output/Video"), ("node.name", "spout2pw.Cam")])));
        assert!(is_spout2pw(&node(&[("media.class", "Stream/Output/Video"), ("node.name", "spout2pw.Game.Cam")])));
        assert!(is_spout2pw(&node(&[("media.class", "Stream/Output/Video"), ("node.name", "x"), ("application.id", "spout2pw.Game")])));
        assert!(!is_spout2pw(&node(&[("media.class", "Stream/Input/Video"), ("node.name", "spout2pw.Cam")])));
        assert!(!is_spout2pw(&node(&[("media.class", "Video/Source"), ("node.name", "spout2pw.Cam")])));
        assert!(!is_spout2pw(&node(&[("media.class", "Stream/Output/Video"), ("node.name", "spout2pwx.Cam")])));
        assert!(!is_spout2pw(&node(&[("media.class", "Stream/Output/Video"), ("node.name", "obs.screen")])));
        let named = node(&[("node.name", "spout2pw.My_Cam"), ("node.description", "Spout2PW: My Cam")]);
        assert_eq!(sender_name(&named), "My Cam");
    }
}
