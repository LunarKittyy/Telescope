//! The phone's microphone as it arrives (/v1/audio: 48 kHz mono s16le), for the audio crate's worker (audio.py's
//! reading half), and the reason the phone gives when it won't send it.

use crate::https::{Aborter, PhoneAuth, Request, Response};
use std::io::Read;
use std::time::Duration;
use telescope_core::audio::{AudioFeed, AudioPacket};
use telescope_core::i18n::tr;
use telescope_core::media::{FeedEnd, FeedStopper};

const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
const READ_SIZE: usize = 4096;

/// /v1/audio's PCM in the chunks it arrives in.
pub struct AudioStream {
    resp: Response,
    buf: Vec<u8>,
}

impl AudioFeed for AudioStream {
    fn read(&mut self) -> Result<AudioPacket, FeedEnd> {
        match self.resp.read(&mut self.buf) {
            Ok(0) => Err(FeedEnd::Closed),
            Ok(n) => Ok(AudioPacket { data: self.buf[..n].to_vec(), time_ns: telescope_core::time::monotonic_ns() }),
            Err(e) => Err(FeedEnd::Error(e.to_string())),
        }
    }

    fn stopper(&self) -> Option<Box<dyn FeedStopper>> {
        Some(Box::new(self.resp.aborter()))
    }
}

impl FeedStopper for Aborter {
    fn stop(&self) {
        self.abort();
    }
}

/// Opens url ("<stream base>/audio"); the error is what the Microphone card shows.
pub fn open_audio(url: &str, auth: &PhoneAuth) -> Result<AudioStream, String> {
    let resp = auth.open_any(Request::get(url, OPEN_TIMEOUT)).map_err(|_| tr("Can't reach the phone's microphone."))?;
    if !(200..300).contains(&resp.status) {
        return Err(refusal(resp));
    }
    Ok(AudioStream { resp, buf: vec![0; READ_SIZE] })
}

/// _phone_reason: the phone's {"code", "error"} as a sentence of this app's.
fn refusal(mut resp: Response) -> String {
    let status = resp.status;
    match resp.read_capped(READ_SIZE).map_err(|_| ()).and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).map_err(|_| ())) {
        Ok(body) => {
            let english = body.get("error").and_then(|e| e.as_str()).filter(|e| !e.is_empty()).map(str::to_string)
                .unwrap_or_else(|| format!("HTTP {status}"));
            phone_mic_reason(body.get("code").and_then(|c| c.as_str()), &english)
        }
        Err(()) => tr("The phone refused the microphone (HTTP {code}).").replace("{code}", &status.to_string()),
    }
}

/// Why the phone's microphone can't be used, from its code; a phone that sends none, or one this app doesn't know,
/// gets its English as it is.
pub fn phone_mic_reason(code: Option<&str>, english: &str) -> String {
    match code {
        Some("mic_none") => tr("No microphone"),
        Some("mic_not_streaming") => tr("Not streaming"),
        Some("mic_permission") => tr("Allow the microphone in Telescope on the phone"),
        Some("mic_open_app") => tr("Open Telescope on the phone once, then try again"),
        Some("mic_in_use") => tr("The phone's microphone is in use"),
        _ => english.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::https::testing::*;
    use std::sync::Arc;

    #[test]
    fn stopping_ends_a_read_on_a_silent_phone() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let _ = conn.read(&mut [0u8; 1024]);
            conn.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
            std::thread::sleep(Duration::from_secs(10));
        });
        let mut feed = open_audio(&format!("http://127.0.0.1:{port}/v1/audio"), &PhoneAuth::new("t", "")).unwrap();
        let stopper = feed.stopper().unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            stopper.stop();
        });
        let started = std::time::Instant::now();
        assert!(feed.read().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_microphone_streams_its_pcm_with_the_token() {
        let server = serve(Some("phone"), Arc::new(|_req| (200, "audio/L16".into(), vec![1u8; 9000])));
        let auth = PhoneAuth::new("tok", &pin_of("phone"));
        let mut feed = open_audio(&format!("{}/v1/audio", server.base(true)), &auth).unwrap();
        let mut got = 0;
        loop {
            match feed.read() {
                Ok(p) => got += p.data.len(),
                Err(FeedEnd::Closed) => break,
                Err(e) => panic!("{e:?}"),
            }
        }
        assert_eq!(got, 9000);
        let seen = server.seen.lock().unwrap().clone();
        assert_eq!((seen[0].path.as_str(), seen[0].auth.as_deref()), ("/v1/audio", Some("Bearer tok")));
    }

    #[test]
    fn a_refused_microphone_says_the_phones_reason() {
        let server = serve(Some("phone"), Arc::new(|_req| (409, "application/json".into(), br#"{"code": "mic_permission", "error": "no permission"}"#.to_vec())));
        let auth = PhoneAuth::new("tok", &pin_of("phone"));
        assert_eq!(open_audio(&format!("{}/v1/audio", server.base(true)), &auth).err().unwrap(), "Allow the microphone in Telescope on the phone");
        let unknown = serve(Some("phone"), Arc::new(|_req| (409, "application/json".into(), br#"{"code": "mic_new", "error": "Something new"}"#.to_vec())));
        assert_eq!(open_audio(&format!("{}/v1/audio", unknown.base(true)), &auth).err().unwrap(), "Something new");
        let plain = serve(Some("phone"), Arc::new(|_req| (503, "text/plain".into(), b"busy".to_vec())));
        assert_eq!(open_audio(&format!("{}/v1/audio", plain.base(true)), &auth).err().unwrap(), "The phone refused the microphone (HTTP 503).");
        assert_eq!(open_audio("https://127.0.0.1:1/v1/audio", &auth).err().unwrap(), "Can't reach the phone's microphone.");
    }
}
