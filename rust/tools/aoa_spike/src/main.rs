// Throwaway AOA bench: switches a phone into accessory mode, then measures a framed bulk link
// Frames: 0xA5, type u8, stream u16 LE, len u32 LE, payload. A frame never exceeds 16384 bytes.
use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, In, Out, Recipient};
use nusb::MaybeFuture;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BATCH: usize = 16384;
const HDR: usize = 8;
const MAX_PAYLOAD: usize = BATCH - HDR;

const T_PING: u8 = 1;
const T_PONG: u8 = 2;
const T_SINK: u8 = 3;
const T_SOURCE: u8 = 4;
const T_DATA: u8 = 5;
const T_DONE: u8 = 6;
const T_REPORT: u8 = 7;
const T_REPORTED: u8 = 8;
const T_OPEN: u8 = 9;
const T_TDATA: u8 = 10;
const T_CLOSE: u8 = 11;
const T_SIZE: u8 = 12;
const T_SIZED: u8 = 13;
const T_RESET: u8 = 14;

trait Tx: Send {
    fn send(&mut self, b: &[u8]) -> io::Result<()>;
    // Ends a batch so the far side's pending read completes
    fn end(&mut self) -> io::Result<()>;
    fn end_no_zlp(&mut self) -> io::Result<()>;
}

struct UsbTx(nusb::io::EndpointWrite<Bulk>);

impl Tx for UsbTx {
    fn send(&mut self, b: &[u8]) -> io::Result<()> {
        self.0.write_all(b)
    }
    fn end(&mut self) -> io::Result<()> {
        self.0.flush_end().map_err(io::Error::other)
    }
    fn end_no_zlp(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

struct TcpTx(TcpStream);

impl Tx for TcpTx {
    fn send(&mut self, b: &[u8]) -> io::Result<()> {
        self.0.write_all(b)
    }
    fn end(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn end_no_zlp(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn frame(t: u8, stream: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(HDR + payload.len());
    v.push(0xA5);
    v.push(t);
    v.extend_from_slice(&stream.to_le_bytes());
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

#[derive(Default)]
struct Shared {
    data_bytes: AtomicU64,
    data_first_ns: AtomicU64,
    data_last_ns: AtomicU64,
    streams: Mutex<HashMap<u16, TcpStream>>,
}

enum Ev {
    Pong(u64),
    Done(u64),
    Reported(u64),
    Sized(usize),
    Gone,
}

struct Link {
    tx: Mutex<Box<dyn Tx>>,
    ev: Mutex<Receiver<Ev>>,
    sh: Arc<Shared>,
    epoch: Instant,
}

impl Link {
    fn send_now(&self, t: u8, stream: u16, p: &[u8]) -> io::Result<()> {
        let mut tx = self.tx.lock().unwrap();
        tx.send(&frame(t, stream, p))?;
        tx.end()
    }
}

fn read_loop(mut r: Box<dyn Read + Send>, ev: Sender<Ev>, sh: Arc<Shared>, epoch: Instant) {
    let mut hdr = [0u8; HDR];
    let mut buf = vec![0u8; BATCH];
    loop {
        if r.read_exact(&mut hdr).is_err() {
            let _ = ev.send(Ev::Gone);
            return;
        }
        if hdr[0] != 0xA5 {
            eprintln!("desync: {:02x?}", hdr);
            let _ = ev.send(Ev::Gone);
            return;
        }
        let t = hdr[1];
        let stream = u16::from_le_bytes([hdr[2], hdr[3]]);
        let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        if len > buf.len() || r.read_exact(&mut buf[..len]).is_err() {
            let _ = ev.send(Ev::Gone);
            return;
        }
        match t {
            T_PONG => {
                let sent = u64::from_le_bytes(buf[..8].try_into().unwrap());
                let _ = ev.send(Ev::Pong(epoch.elapsed().as_nanos() as u64 - sent));
            }
            T_DATA => {
                let now = epoch.elapsed().as_nanos() as u64;
                let _ = sh.data_first_ns.compare_exchange(0, now, Ordering::SeqCst, Ordering::SeqCst);
                sh.data_last_ns.store(now, Ordering::SeqCst);
                sh.data_bytes.fetch_add(len as u64, Ordering::SeqCst);
            }
            T_DONE => {
                let _ = ev.send(Ev::Done(u64::from_le_bytes(buf[..8].try_into().unwrap())));
            }
            T_REPORTED => {
                let _ = ev.send(Ev::Reported(u64::from_le_bytes(buf[..8].try_into().unwrap())));
            }
            T_SIZED => {
                let _ = ev.send(Ev::Sized(len));
            }
            T_TDATA => {
                let mut m = sh.streams.lock().unwrap();
                if let Some(s) = m.get_mut(&stream) {
                    let _ = s.write_all(&buf[..len]);
                }
            }
            T_CLOSE => {
                if let Some(s) = sh.streams.lock().unwrap().remove(&stream) {
                    let _ = s.shutdown(Shutdown::Both);
                }
            }
            other => eprintln!("unknown frame {other}"),
        }
    }
}

fn wait_ev<T>(l: &Link, timeout: Duration, mut pick: impl FnMut(Ev) -> Option<T>) -> Option<T> {
    let end = Instant::now() + timeout;
    let rx = l.ev.lock().unwrap();
    loop {
        let left = end.checked_duration_since(Instant::now())?;
        match rx.recv_timeout(left) {
            Ok(Ev::Gone) => {
                println!("link gone");
                return None;
            }
            Ok(e) => {
                if let Some(v) = pick(e) {
                    return Some(v);
                }
            }
            Err(RecvTimeoutError::Timeout) => return None,
            Err(_) => return None,
        }
    }
}

fn ping_once(l: &Link) -> Option<f64> {
    let t = l.epoch.elapsed().as_nanos() as u64;
    l.send_now(T_PING, 0, &t.to_le_bytes()).ok()?;
    wait_ev(l, Duration::from_secs(2), |e| if let Ev::Pong(ns) = e { Some(ns as f64 / 1e6) } else { None })
}

fn stats(name: &str, v: &mut [f64]) {
    if v.is_empty() {
        println!("{name}: no samples");
        return;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
    println!(
        "{name}: n={} min={:.3} median={:.3} p99={:.3} max={:.3} ms",
        v.len(),
        v[0],
        q(0.5),
        q(0.99),
        v[v.len() - 1]
    );
}

fn pings(l: &Link, name: &str, n: usize, gap: Duration) {
    let mut v = Vec::new();
    for _ in 0..n {
        if let Some(ms) = ping_once(l) {
            v.push(ms);
        }
        std::thread::sleep(gap);
    }
    stats(name, &mut v);
}

fn source(l: &Link, burst: u32, interval_ms: u32, secs: u32) {
    let mut p = Vec::new();
    p.extend_from_slice(&burst.to_le_bytes());
    p.extend_from_slice(&interval_ms.to_le_bytes());
    p.extend_from_slice(&secs.to_le_bytes());
    l.send_now(T_SOURCE, 0, &p).unwrap();
}

fn reset_data(l: &Link) {
    l.sh.data_bytes.store(0, Ordering::SeqCst);
    l.sh.data_first_ns.store(0, Ordering::SeqCst);
}

fn data_mbps(l: &Link) -> f64 {
    let b = l.sh.data_bytes.load(Ordering::SeqCst) as f64;
    let d = (l.sh.data_last_ns.load(Ordering::SeqCst) - l.sh.data_first_ns.load(Ordering::SeqCst)) as f64 / 1e9;
    b * 8.0 / d / 1e6
}

fn up(l: &Link, secs: u64) -> f64 {
    let f = frame(T_SINK, 0, &vec![0x5A; MAX_PAYLOAD]);
    let t0 = Instant::now();
    let mut n = 0u64;
    while t0.elapsed() < Duration::from_secs(secs) {
        let mut tx = l.tx.lock().unwrap();
        for _ in 0..16 {
            if tx.send(&f).is_err() {
                return 0.0;
            }
            n += 1;
        }
    }
    let _ = l.send_now(T_REPORT, 0, &[]);
    let got = wait_ev(l, Duration::from_secs(5), |e| if let Ev::Reported(b) = e { Some(b) } else { None });
    let dt = t0.elapsed().as_secs_f64();
    let sent = n * MAX_PAYLOAD as u64;
    println!("up raw: sent {sent} phone counted {got:?} in {dt:.2}s");
    got.unwrap_or(0) as f64 * 8.0 / dt / 1e6
}

fn down(l: &Link, secs: u32) -> f64 {
    reset_data(l);
    source(l, MAX_PAYLOAD as u32, 0, secs);
    let d = wait_ev(l, Duration::from_secs(secs as u64 + 10), |e| if let Ev::Done(b) = e { Some(b) } else { None });
    println!("down raw: phone sent {d:?} host got {}", l.sh.data_bytes.load(Ordering::SeqCst));
    data_mbps(l)
}

fn sizes(l: &Link) {
    for total in [512usize, 1024, 8192, 16384] {
        let plen = total - HDR;
        let req = (plen as u32).to_le_bytes();
        // Phone to host: phone writes one frame of exactly this size
        l.send_now(T_SIZE, 0, &req).unwrap();
        let down = wait_ev(l, Duration::from_millis(700), |e| if let Ev::Sized(n) = e { Some(n) } else { None });
        // Host to phone with a ZLP where needed
        let t0 = Instant::now();
        {
            let mut tx = l.tx.lock().unwrap();
            tx.send(&frame(T_SIZE, 1, &vec![0u8; plen])).unwrap();
            tx.end().unwrap();
        }
        let zlp = wait_ev(l, Duration::from_millis(700), |e| if let Ev::Sized(n) = e { Some(n) } else { None });
        let zlp_ms = t0.elapsed().as_secs_f64() * 1e3;
        // Host to phone without one: the phone's read may stay pending
        let t0 = Instant::now();
        {
            let mut tx = l.tx.lock().unwrap();
            tx.send(&frame(T_SIZE, 1, &vec![0u8; plen])).unwrap();
            tx.end_no_zlp().unwrap();
        }
        let plain = wait_ev(l, Duration::from_millis(700), |e| if let Ev::Sized(n) = e { Some(n) } else { None });
        let plain_ms = t0.elapsed().as_secs_f64() * 1e3;
        println!("size {total}: phone-to-host {down:?}; host-to-phone with ZLP {zlp:?} {zlp_ms:.1} ms; without ZLP {plain:?} {plain_ms:.1} ms");
        // A short frame clears whatever was left pending
        let _ = ping_once(l);
        while wait_ev(l, Duration::from_millis(100), |e| if let Ev::Sized(n) = e { Some(n) } else { None }).is_some() {}
    }
}

fn bench(l: &Arc<Link>, secs: u32, label: &str) {
    println!("=== bench over {label}, {secs}s per run, {BATCH} byte batches");
    pings(l, "idle rtt", 2000, Duration::from_millis(2));
    println!("up: {:.1} Mbps", up(l, secs as u64));
    std::thread::sleep(Duration::from_millis(300));
    println!("down: {:.1} Mbps", down(l, secs));
    std::thread::sleep(Duration::from_millis(300));
    // 20 Mbps at 30 fps: one 83 KB frame burst every 33 ms
    reset_data(l);
    source(l, 83_333, 33, secs);
    pings(l, "rtt under 20 Mbps phone-to-host", (secs * 200) as usize, Duration::from_millis(5));
    let _ = wait_ev(l, Duration::from_secs(5), |e| if let Ev::Done(b) = e { Some(b) } else { None });
    println!("load achieved: {:.1} Mbps", data_mbps(l));
    // Full duplex at once
    reset_data(l);
    source(l, MAX_PAYLOAD as u32, 0, secs);
    let u = up(l, secs as u64);
    let _ = wait_ev(l, Duration::from_secs(5), |e| if let Ev::Done(b) = e { Some(b) } else { None });
    println!("duplex: up {:.1} Mbps, down {:.1} Mbps", u, data_mbps(l));
}

fn tunnel_accept(l: Arc<Link>, listen: u16, target: u16, ids: Arc<AtomicU16>) {
    let srv = TcpListener::bind(("127.0.0.1", listen)).unwrap();
    println!("tunnel 127.0.0.1:{listen} -> phone 127.0.0.1:{target}");
    for s in srv.incoming().flatten() {
        let _ = s.set_nodelay(true);
        let id = ids.fetch_add(1, Ordering::SeqCst);
        l.sh.streams.lock().unwrap().insert(id, s.try_clone().unwrap());
        l.send_now(T_OPEN, id, &target.to_le_bytes()).unwrap();
        let l = l.clone();
        std::thread::spawn(move || {
            let mut s = s;
            let mut buf = vec![0u8; MAX_PAYLOAD];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if l.send_now(T_TDATA, id, &buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = l.send_now(T_CLOSE, id, &[]);
            l.sh.streams.lock().unwrap().remove(&id);
        });
    }
}

fn find_accessory() -> Option<nusb::DeviceInfo> {
    nusb::list_devices().wait().ok()?.find(|d| d.vendor_id() == 0x18d1 && matches!(d.product_id(), 0x2d00 | 0x2d01))
}

#[cfg(windows)]
fn switch_to_accessory() -> Result<Duration, String> {
    Err("use switch-intf on Windows".into())
}

#[cfg(not(windows))]
fn switch_to_accessory() -> Result<Duration, String> {
    let di = nusb::list_devices().wait().map_err(|e| e.to_string())?.find(|d| d.vendor_id() == 0x04e8).ok_or("no phone")?;
    println!("found {:04x}:{:04x}", di.vendor_id(), di.product_id());
    let dev = di.open().wait().map_err(|e| format!("open: {e}"))?;
    let t = Duration::from_secs(2);
    let ver = dev
        .control_in(ControlIn { control_type: ControlType::Vendor, recipient: Recipient::Device, request: 51, value: 0, index: 0, length: 2 }, t)
        .wait()
        .map_err(|e| format!("req 51: {e}"))?;
    println!("AOA protocol {}", u16::from_le_bytes([ver[0], ver[1]]));
    for (i, s) in ["Telescope", "AoaSpike", "Telescope USB link spike", "0.1", "", "spike0001"].iter().enumerate() {
        if s.is_empty() {
            continue;
        }
        let mut d = s.as_bytes().to_vec();
        d.push(0);
        dev.control_out(ControlOut { control_type: ControlType::Vendor, recipient: Recipient::Device, request: 52, value: 0, index: i as u16, data: &d }, t)
            .wait()
            .map_err(|e| format!("req 52: {e}"))?;
    }
    let t0 = Instant::now();
    dev.control_out(ControlOut { control_type: ControlType::Vendor, recipient: Recipient::Device, request: 53, value: 0, index: 0, data: &[] }, t)
        .wait()
        .map_err(|e| format!("req 53: {e}"))?;
    while t0.elapsed() < Duration::from_secs(10) {
        if find_accessory().is_some() {
            return Ok(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err("no accessory after 10 s".into())
}

// Windows path: the requests go out through one claimed WinUSB interface of the composite device
fn switch_via_interface(n: u8) -> Result<(), String> {
    let di = nusb::list_devices().wait().map_err(|e| e.to_string())?.find(|d| d.vendor_id() == 0x04e8).ok_or("no phone")?;
    println!("found {:04x}:{:04x}", di.vendor_id(), di.product_id());
    let dev = di.open().wait().map_err(|e| format!("open: {e}"))?;
    let intf = dev.claim_interface(n).wait().map_err(|e| format!("claim {n}: {e}"))?;
    println!("claimed interface {n}");
    let t = Duration::from_secs(2);
    let ver = intf
        .control_in(ControlIn { control_type: ControlType::Vendor, recipient: Recipient::Device, request: 51, value: 0, index: 0, length: 2 }, t)
        .wait()
        .map_err(|e| format!("req 51: {e}"))?;
    println!("AOA protocol {}", u16::from_le_bytes([ver[0], ver[1]]));
    for (i, s) in ["Telescope", "AoaSpike", "Telescope USB link spike", "0.1", "", "spike0001"].iter().enumerate() {
        if s.is_empty() {
            continue;
        }
        let mut d = s.as_bytes().to_vec();
        d.push(0);
        intf.control_out(ControlOut { control_type: ControlType::Vendor, recipient: Recipient::Device, request: 52, value: 0, index: i as u16, data: &d }, t)
            .wait()
            .map_err(|e| format!("req 52: {e}"))?;
    }
    intf.control_out(ControlOut { control_type: ControlType::Vendor, recipient: Recipient::Device, request: 53, value: 0, index: 0, data: &[] }, t)
        .wait()
        .map_err(|e| format!("req 53: {e}"))?;
    println!("start sent");
    Ok(())
}

fn open_usb() -> Result<Arc<Link>, String> {
    let t0 = Instant::now();
    let di = match find_accessory() {
        Some(d) => d,
        None => {
            let d = switch_to_accessory()?;
            println!("re-enumerated after {:.0} ms", d.as_secs_f64() * 1e3);
            find_accessory().ok_or("vanished")?
        }
    };
    println!("device {:04x}:{:04x} speed {:?}", di.vendor_id(), di.product_id(), di.speed());
    let mut last = String::new();
    for _ in 0..200 {
        match di.open().wait().and_then(|d| d.claim_interface(0).wait()) {
            Ok(intf) => {
                println!("claimed after {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);
                let tx = intf.endpoint::<Bulk, Out>(0x01).map_err(|e| e.to_string())?.writer(BATCH).with_num_transfers(4);
                let rx = intf.endpoint::<Bulk, In>(0x81).map_err(|e| e.to_string())?.reader(BATCH).with_num_transfers(8);
                return Ok(make_link(Box::new(UsbTx(tx)), Box::new(rx)));
            }
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    Err(format!("claim: {last}"))
}

fn open_tcp(port: u16) -> Result<Arc<Link>, String> {
    let s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    s.set_nodelay(true).map_err(|e| e.to_string())?;
    let r = s.try_clone().map_err(|e| e.to_string())?;
    Ok(make_link(Box::new(TcpTx(s)), Box::new(r)))
}

fn make_link(tx: Box<dyn Tx>, rx: Box<dyn Read + Send>) -> Arc<Link> {
    let (etx, erx) = channel();
    let sh = Arc::new(Shared::default());
    let epoch = Instant::now();
    let sh2 = sh.clone();
    std::thread::spawn(move || read_loop(rx, etx, sh2, epoch));
    let l = Arc::new(Link { tx: Mutex::new(tx), ev: Mutex::new(erx), sh, epoch });
    let _ = l.send_now(T_RESET, 0, &[]);
    l
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cmd = a.get(1).map(String::as_str).unwrap_or("");
    let num = |i: usize, d: u32| a.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let usb = || open_usb().unwrap_or_else(|e| panic!("{e}"));
    match cmd {
        "switch" => println!("{:?}", switch_to_accessory()),
        "list" => {
            for d in nusb::list_devices().wait().unwrap() {
                println!("{:04x}:{:04x} {:?} {:?}", d.vendor_id(), d.product_id(), d.product_string(), d.interfaces().map(|i| i.interface_number()).collect::<Vec<_>>());
            }
        }
        "switch-intf" => println!("{:?}", switch_via_interface(num(2, 3) as u8)),
        "reset" => {
            let d = find_accessory().expect("no accessory").open().wait().expect("open");
            #[cfg(not(windows))]
            println!("{:?}", d.reset().wait());
            #[cfg(windows)]
            drop(d);
        }
        "ping" => pings(&usb(), "idle rtt", num(2, 200) as usize, Duration::from_millis(2)),
        "sizes" => sizes(&usb()),
        "bench" => bench(&usb(), num(2, 10), "USB bulk"),
        "bench-tcp" => bench(&open_tcp(num(2, 18777) as u16).unwrap(), num(3, 10), "adb forward"),
        "tunnel" => {
            let l = usb();
            let ids = Arc::new(AtomicU16::new(1));
            for (lp, rp) in [(18080u16, 8080u16), (18766, 8766)] {
                let (l, ids) = (l.clone(), ids.clone());
                std::thread::spawn(move || tunnel_accept(l, lp, rp, ids));
            }
            std::thread::sleep(Duration::from_secs(num(2, 30) as u64));
        }
        // Keeps pinging and prints every gap, for unplug and lifecycle tests
        "watch" => {
            let l = usb();
            let t0 = Instant::now();
            let mut bad = 0;
            source(&l, 83_333, 33, num(2, 30));
            while t0.elapsed() < Duration::from_secs(num(2, 30) as u64) {
                match ping_once(&l) {
                    Some(ms) if ms > 20.0 => println!("{:.2}s slow ping {ms:.1} ms", t0.elapsed().as_secs_f64()),
                    Some(_) => bad = 0,
                    None => {
                        println!("{:.2}s ping failed", t0.elapsed().as_secs_f64());
                        bad += 1;
                        if bad > 3 {
                            return;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            println!("watch ended, link alive");
        }
        _ => eprintln!("switch | ping [n] | sizes | bench [secs] | bench-tcp [port] [secs] | tunnel [secs] | watch [secs]"),
    }
}
