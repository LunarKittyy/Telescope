//! Small shared helpers: random tokens, hashes, names.

use base64::Engine;
use ring::rand::SecureRandom;

/// Like Python's secrets.token_urlsafe(n): n random bytes, base64url without padding.
pub fn token_urlsafe(n: usize) -> String {
    let mut buf = vec![0u8; n];
    ring::rand::SystemRandom::new().fill(&mut buf).expect("system random");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    ring::rand::SystemRandom::new().fill(&mut buf).expect("system random");
    hex(&buf)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
}

pub fn is_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// The HMAC the phone sends as `proof` when it pairs (pairing.py pairing_proof).
pub fn pairing_proof(token: &str, nonce: &str, phone_id: &str, cert_sha256: &str) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, token.as_bytes());
    let message = format!("telescope-pair-v4\n{nonce}\n{phone_id}\n{cert_sha256}");
    hex(ring::hmac::sign(&key, message.as_bytes()).as_ref())
}

/// Constant-time string comparison for proofs and pins.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// Shared with the Presets plugin, so it lives in core
pub use telescope_core::names::{clean_name, MAX_NAME_CHARS};

pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: buf is valid for its length and gethostname NUL-terminates within it on success
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
        let end = buf.iter().position(|&b| b == 0).unwrap_or(0);
        if ok && end > 0 { String::from_utf8_lossy(&buf[..end]).into_owned() } else { "Computer".into() }
    }
    #[cfg(not(unix))]
    std::env::var("COMPUTERNAME").ok().filter(|n| !n.is_empty()).unwrap_or_else(|| "Computer".into())
}

/// Standard base64 with padding, how the USB pairing offer travels through adb.
pub fn base64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// json.dumps(value): the bytes the Python app sends, so the phone traffic matches.
pub fn python_json(value: &serde_json::Value) -> String {
    use telescope_core::json::{dumps, Style};
    dumps(value, Style { indent: None, sort_keys: false, ensure_ascii: true })
}

/// json.dumps(value, ensure_ascii=False), how control bodies go out.
pub fn python_json_utf8(value: &serde_json::Value) -> String {
    use telescope_core::json::{dumps, Style};
    dumps(value, Style { indent: None, sort_keys: false, ensure_ascii: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_matches_python() {
        // python -c "from telescope.pairing import pairing_proof; print(pairing_proof('tok','n','p','c'*64))"
        assert_eq!(
            pairing_proof("tok", "n", "p", &"c".repeat(64)),
            "712d49281e78f3ac359d72d1e855fa7c535cd0111127c1cc3b3cfc37b407e94e"
        );
    }

    #[test]
    fn clean_name_strips_markup() {
        assert_eq!(clean_name("  <b>Pixel\n8</b> "), "bPixel 8/b");
    }

    #[test]
    fn phone_names_are_cleaned_into_plain_display_text() {
        assert_eq!(clean_name("Sam's Pixel"), "Sam's Pixel");
        assert_eq!(clean_name("<b>Your bank</b>\n\x07phone"), "bYour bank/b phone");
        assert_eq!(clean_name(&"x".repeat(500)).chars().count(), MAX_NAME_CHARS);
        assert_eq!(clean_name("  \t "), "");
        assert_eq!(clean_name("a\u{202e}b\u{1c}c"), "ab c");
    }

    #[test]
    fn the_proof_matches_the_phone_app() {
        // Same vector as the phone's PairingTest.kt
        assert_eq!(pairing_proof("tok-123", "nonce-abc", "phone-1", &"ab".repeat(32)),
                   "fabf87423641e2c832e78a0385ce388ccadc57e896fe2b028586b22c2873d915");
    }
}
