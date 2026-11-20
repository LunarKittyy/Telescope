use super::*;

fn phone(token: &str) -> PairedPhone {
    PairedPhone { id: "phone".into(), name: "Phone".into(), token: token.into(), ips: vec!["192.168.1.20".into()], active_ip: None, cert_sha256: String::new() }
}

fn engine() -> Engine {
    let mut engine = Engine::default();
    engine.configure(Config { id: "desktop".into(), version: "3.6.0".into(), phones: vec![phone("secret")] }, 0.0);
    engine.local = vec!["192.168.1.10".into()];
    engine.peers.get_mut("phone").unwrap().capable = true;
    engine
}

fn nonce(n: u32) -> String { format!("{n:032x}") }
fn source() -> SocketAddr { "192.168.1.20:8769".parse().unwrap() }
fn local() -> IpAddr { "192.168.1.10".parse().unwrap() }

#[derive(Default)]
struct FakeTransport(Vec<Send>);
impl Transport for FakeTransport {
    fn send(&mut self, send: Send) { self.0.push(send); }
}

#[test]
fn fake_clock_visits_each_schedule_phase_and_coalesces_hints() {
    let mut schedule = Schedule::new(0.0);
    assert!(schedule.due(0.0, 0.5));
    assert!(!schedule.due(1.9, 0.5));
    assert!(schedule.due(2.0, 0.5));
    assert!(schedule.due(10.0, 0.5));
    assert_eq!(schedule.next, 15.0);
    assert!(schedule.due(180.0, 0.5));
    assert_eq!(schedule.next, 225.0);
    schedule.hint(181.0);
    assert!(schedule.due(181.0, 0.5));
    schedule.hint(181.1);
    assert!(!schedule.due(181.1, 0.5));
    assert!(schedule.due(183.0, 0.5));
}

#[test]
fn mac_covers_all_fields_and_never_contains_the_token() {
    let encoded = encode("q", "phone", "desktop", &nonce(1), "3.6.0", &["192.168.1.20".into()], "secret").unwrap();
    assert!(!String::from_utf8_lossy(&encoded).contains("secret"));
    assert!(String::from_utf8_lossy(&encoded).ends_with("4ffb45918a4d12c2148314eb126b1eb4416743880ce41e4453c338836814f1f9"));
    assert!(Packet::parse(&encoded).unwrap().authenticated("secret"));
    assert!(!Packet::parse(&encoded).unwrap().authenticated("other"));
    for (old, new) in [("TP1", "TP2"), ("\nq\n", "\na\n"), ("phone", "other"), ("desktop", "another"), ("3.6.0", "3.7.0"), ("192.168.1.20", "192.168.1.21")] {
        let changed = String::from_utf8(encoded.clone()).unwrap().replace(old, new);
        assert!(!Packet::parse(changed.as_bytes()).is_some_and(|packet| packet.authenticated("secret")));
    }
}

#[test]
fn unsolicited_answers_expiry_and_replays_do_not_prove_reachability() {
    let mut engine = engine();
    let answer = encode("a", "phone", "desktop", &nonce(1), "3.7.0", &[], "secret").unwrap();
    assert!(engine.receive(&answer, source(), local(), 0.0, nonce(2)).1.is_none());
    engine.question("phone", source(), Some(local()), 0.0, nonce(1)).unwrap();
    let event = engine.receive(&answer, source(), local(), 1.0, nonce(2)).1.unwrap();
    assert_eq!(event, PresenceEvent::Reachable { phone_id: "phone".into(), address: "192.168.1.20".into(), addresses: vec![], version: "3.7.0".into() });
    assert!(engine.receive(&answer, source(), local(), 2.0, nonce(2)).1.is_none());
    let request = encode("q", "phone", "desktop", &nonce(4), "3.7.0", &[], "secret").unwrap();
    assert_eq!(engine.receive(&request, source(), local(), 2.0, nonce(5)).0.len(), 1, "a confirmed return path does not start a reply loop");
    engine.question("phone", source(), Some(local()), 2.0, nonce(1)).unwrap();
    assert!(engine.receive(&answer, source(), local(), 12.0, nonce(2)).1.is_none());
}

#[test]
fn request_gets_bounded_answer_and_return_challenge_without_changing_schedule() {
    let mut engine = engine();
    let request = encode("q", "phone", "desktop", &nonce(1), "", &[], "secret").unwrap();
    let mut transport = FakeTransport::default();
    let (sends, event) = engine.receive(&request, source(), local(), 50.0, nonce(2));
    assert!(event.is_none());
    dispatch(&mut transport, sends);
    assert_eq!(transport.0.len(), 2);
    assert!(transport.0.iter().all(|send| send.local == Some(local()) && send.target == source()));
    assert_eq!(engine.peers["phone"].schedule.interacted, 0.0);
    assert!(engine.receive(&request, source(), local(), 52.0, nonce(3)).0.is_empty());
    assert!(engine.receive(&request, source(), local(), 52.0, nonce(3)).1.is_none());
}

#[test]
fn revocation_and_token_rotation_invalidate_pending_and_replay_state() {
    let mut engine = engine();
    engine.question("phone", source(), None, 0.0, nonce(1));
    engine.configure(Config { id: "desktop".into(), version: "3.6.0".into(), phones: vec![phone("new-secret")] }, 1.0);
    assert!(engine.pending.is_empty());
    let old = encode("a", "phone", "desktop", &nonce(1), "3.6.0", &[], "secret").unwrap();
    assert!(engine.receive(&old, source(), local(), 2.0, nonce(2)).1.is_none());
    engine.configure(Config { id: "desktop".into(), version: "3.6.0".into(), phones: vec![] }, 3.0);
    assert!(engine.pending.is_empty());
    assert!(engine.receive(&old, source(), local(), 4.0, nonce(2)).1.is_none());
}

#[test]
fn unknown_and_legacy_capabilities_send_nothing_but_still_allow_authenticated_answers() {
    let mut engine = engine();
    engine.peers.get_mut("phone").unwrap().capable = false;
    assert!(engine.tick(0.0, || nonce(1)).is_empty());
    let request = encode("q", "phone", "desktop", &nonce(1), "", &[], "secret").unwrap();
    assert_eq!(engine.receive(&request, source(), local(), 1.0, nonce(2)).0.len(), 2);
    engine.peers.get_mut("phone").unwrap().capable = true;
    let mut transport = FakeTransport::default();
    dispatch(&mut transport, engine.tick(2.0, || nonce(3)));
    assert_eq!(transport.0.len(), 1);
    assert_eq!(transport.0[0].target, source());
}

#[test]
fn a_working_route_is_kept_fresh_while_other_candidates_rotate() {
    let mut engine = engine();
    let peer = engine.peers.get_mut("phone").unwrap();
    peer.phone.ips = vec!["192.168.1.21".into(), "192.168.1.22".into(), "100.64.1.2".into()];
    peer.confirmed = Some((source(), 180.0));
    let mut sequence = 0;
    let sends = engine.tick(180.0, || { sequence += 1; nonce(sequence) });
    assert_eq!(sends.len(), 2);
    assert!(sends.iter().any(|send| send.target == source()));
    assert!(sends.iter().any(|send| send.target.ip().to_string() == "192.168.1.21"));
}

#[test]
fn credential_revision_discards_queued_and_late_events() {
    let controls = Controls::default();
    controls.revision.store(1, Ordering::Release);
    event(&controls, PresenceEvent::Probe { phone_id: "phone".into() }, 1);
    assert_eq!(controls.events.lock().unwrap().len(), 1);
    controls.revision.store(2, Ordering::Release);
    controls.events.lock().unwrap().clear();
    event(&controls, PresenceEvent::Probe { phone_id: "phone".into() }, 1);
    assert!(controls.events.lock().unwrap().is_empty());
    event(&controls, PresenceEvent::Probe { phone_id: "phone".into() }, 2);
    assert_eq!(controls.events.lock().unwrap().len(), 1);
}

#[test]
fn reconfiguration_keeps_schedule_and_version_differences_are_data() {
    let mut engine = engine();
    engine.peers.get_mut("phone").unwrap().schedule.due(0.0, 0.5);
    engine.configure(Config { id: "desktop".into(), version: "3.7.0".into(), phones: vec![phone("secret")] }, 1.0);
    assert_eq!(engine.peers["phone"].schedule.next, 2.0);
    assert_eq!(engine.peers["phone"].schedule.interacted, 0.0);
    let packet = encode("q", "phone", "desktop", &nonce(1), "", &[], "secret").unwrap();
    assert_eq!(Packet::parse(&packet).unwrap().version, "");
    let packet = encode("q", "phone", "desktop", &nonce(1), "3.8.0-nightly.12", &[], "secret").unwrap();
    assert_eq!(Packet::parse(&packet).unwrap().version, "3.8.0-nightly.12");
}

#[test]
fn bounded_candidates_keep_a_tailscale_route() {
    let ips = presence_addresses(["192.168.1.1", "10.0.0.1", "10.1.0.1", "10.2.0.1", "100.64.0.2"].map(str::to_string));
    assert_eq!(ips.len(), 4);
    assert!(ips.contains(&"100.64.0.2".into()));
}

#[test]
fn socket_candidates_keep_all_adapters_while_packets_keep_four_addresses() {
    let scan = || AdapterAddresses::new(["192.168.1.1", "10.0.0.1", "10.1.0.1", "10.2.0.1", "10.3.0.1", "100.64.0.2"].map(str::to_string));
    let initial = scan();
    assert_eq!(initial.bound.len(), 6);
    assert_eq!(initial.advertised.len(), 4);
    assert!(initial.advertised.contains(&"100.64.0.2".into()));
    assert_eq!(scan().bound, initial.bound, "an unchanged scan does not rebuild sockets or reset the fast schedule");
    let mut changed = initial.bound.clone();
    changed[4] = "10.4.0.1".into();
    assert_ne!(AdapterAddresses::new(changed).bound, initial.bound, "changes beyond the advertised four still rebuild sockets");
}

#[test]
fn malformed_oversized_or_unroutable_packets_are_rejected() {
    assert!(Packet::parse(&vec![b'a'; MAX_PACKET + 1]).is_none());
    assert!(encode("q", "bad\nidentity", "desktop", &nonce(1), "3.6.0", &[], "secret").is_none());
    let bad = encode("q", "phone", "desktop", &nonce(1), "3.6.0", &["127.0.0.1".into()], "secret").unwrap();
    assert!(Packet::parse(&bad).is_none());
}
