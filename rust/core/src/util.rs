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

/// A phone's name as safe display text (session_client.clean_name).
pub fn clean_name(raw: &str) -> String {
    let text: String = raw
        .chars()
        .filter(|c| (!c.is_control() || c.is_whitespace()) && *c != '<' && *c != '>')
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(64).collect()
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for its length and gethostname NUL-terminates within it on success
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(0);
    if ok && end > 0 { String::from_utf8_lossy(&buf[..end]).into_owned() } else { "Computer".into() }
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
}

/// CLOCK_MONOTONIC in nanoseconds, the clock Python's time.monotonic() reads, for timing across processes.
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: ts is a valid out-pointer
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
