use super::drm::*;
use super::*;

const PROC_STAT: &str = "cpu  4705 150 1120 16250856 1710 0 84 0 0 0\ncpu0 1393 63 261 8118529 903 0 37 0 0 0\ncpu1 3312 87 859 8132327 807 0 47 0 0 0\nintr 1462898 0 10 0 0 0\nctxt 3256345\nbtime 1700000000\n";

const MEMINFO: &str = "MemTotal:       32768000 kB\nMemFree:         1048576 kB\nMemAvailable:   20971520 kB\nBuffers:          204800 kB\nCached:         16777216 kB\n";

fn raw(pid: u32, name: &str, ticks: u64) -> RawProcess {
    RawProcess { pid, start: 1000, name: name.into(), ticks }
}

fn use_of(name: &str, percent: f32) -> ProcessUse {
    ProcessUse { name: name.into(), percent }
}

#[test]
fn proc_stat_gives_busy_and_all_jiffies() {
    let times = parse_proc_stat(PROC_STAT).unwrap();
    // user 4705 + nice 150 + system 1120 + idle 16250856 + iowait 1710 + softirq 84; idle and iowait aren't busy
    assert_eq!(times.total, 4705 + 150 + 1120 + 16250856 + 1710 + 84);
    assert_eq!(times.busy, 4705 + 150 + 1120 + 84);
    assert!(parse_proc_stat("cpu0 1 2 3 4\n").is_none());
    assert!(parse_proc_stat("").is_none());
    // An old kernel has four columns only
    assert_eq!(parse_proc_stat("cpu  10 0 5 85\n"), Some(CpuTimes { busy: 15, total: 100 }));
}

#[test]
fn a_pid_stat_is_read_past_a_name_with_spaces_and_brackets() {
    let rest = "S 1 2 3 4 5 6 7 8 9 10 250 70 0 0 20 0 4 0 987654 1000000 2000 18446744073709551615";
    let (a, b) = (format!("4242 (firefox) {rest}"), format!("77 (Web Content (v2)) {rest}"));
    let plain = parse_pid_stat(&a).unwrap();
    assert_eq!(plain, PidStat { comm: "firefox", ticks: 320, start: 987654 });
    let odd = parse_pid_stat(&b).unwrap();
    assert_eq!((odd.comm, odd.ticks, odd.start), ("Web Content (v2)", 320, 987654));
    assert!(parse_pid_stat("77 (cut off) S 1 2").is_none());
    assert!(parse_pid_stat("garbage").is_none());
}

#[test]
fn meminfo_gives_total_and_available() {
    assert_eq!(parse_meminfo(MEMINFO), Some(MemInfo { total_kb: 32768000, available_kb: Some(20971520) }));
    assert_eq!(parse_meminfo("MemTotal: 1024 kB\n"), Some(MemInfo { total_kb: 1024, available_kb: None }));
    assert_eq!(parse_meminfo("MemFree: 1 kB\n"), None);
}

#[test]
fn a_program_is_named_without_folders_or_extension() {
    assert_eq!(program_name(r"C:\Program Files\Some App\some app.exe"), "some app");
    assert_eq!(program_name(r"C:\Windows\System32\svchost.exe"), "svchost");
    assert_eq!(program_name("/usr/bin/tool"), "tool");
    assert_eq!(program_name("plain"), "plain");
    assert_eq!(program_name(r"D:\x\.hidden"), ".hidden");
}

#[test]
fn graphics_instances_name_the_process_and_engine() {
    assert_eq!(parse_gpu_instance("pid_1234_luid_0x00000000_0x0000A1B2_phys_0_eng_3_engtype_3D"), Some((1234, "3D")));
    assert_eq!(parse_gpu_instance("pid_9_luid_0x00000000_0x0000A1B2_phys_0_eng_0_engtype_VideoDecode"), Some((9, "VideoDecode")));
    assert_eq!(parse_gpu_instance("luid_0x0_engtype_3D"), None);
    assert_eq!(parse_gpu_instance("pid_x_engtype_3D"), None);
}

#[test]
fn the_card_is_as_busy_as_its_busiest_kind_of_work_and_each_process_by_its_own_busiest() {
    let load = gpu_load([
        ("pid_10_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 30.0),
        ("pid_10_luid_0x0_0x1_phys_0_eng_1_engtype_3D", 20.0),
        ("pid_10_luid_0x0_0x1_phys_0_eng_2_engtype_Copy", 5.0),
        ("pid_20_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 40.0),
        ("pid_20_luid_0x0_0x1_phys_0_eng_5_engtype_VideoDecode", 10.0),
        ("not an instance", 99.0),
    ])
    .unwrap();
    assert_eq!(load.total, 90.0, "3D: 30 + 20 + 40, more than the others");
    assert_eq!(load.per_pid, vec![(10, 50.0), (20, 40.0)]);
    let capped = gpu_load([("pid_1_engtype_3D", 80.0), ("pid_2_engtype_3D", 70.0)]).unwrap();
    assert_eq!((capped.total, capped.per_pid), (100.0, vec![(1, 80.0), (2, 70.0)]));
    assert!(gpu_load([("nothing useful", 5.0)]).is_none());
    assert!(gpu_load(std::iter::empty()).is_none());
}

#[test]
fn the_processor_is_busy_by_the_change_between_two_looks() {
    let (a, b) = (CpuTimes { busy: 100, total: 1000 }, CpuTimes { busy: 150, total: 1100 });
    assert_eq!(cpu_percent(a, b), Some(50.0));
    assert_eq!(cpu_percent(b, b), None, "no time passed");
    assert_eq!(cpu_percent(b, a), None, "counters went backwards");
    assert_eq!(cpu_percent(a, CpuTimes { busy: 400, total: 1100 }), Some(100.0), "capped");
}

#[test]
fn the_busiest_programs_are_added_by_name_and_new_processes_have_no_rate_yet() {
    let before: HashMap<(u32, u64), u64> = [((1, 1000), 100), ((2, 1000), 200), ((3, 1000), 50), ((4, 1000), 10)].into();
    let now = [raw(1, "browser", 130), raw(2, "browser", 260), raw(3, "game", 150), raw(4, "idle thing", 10), raw(5, "fresh", 9999)];
    let top = busiest(&before, &now, 400, 3);
    assert_eq!(top, vec![use_of("game", 25.0), use_of("browser", 22.5)]);
    // A process that took over a finished one's number (a different start) is new too
    let reused = [RawProcess { start: 2000, ..raw(1, "other", 5000) }];
    assert!(busiest(&before, &reused, 400, 3).is_empty());
    assert_eq!(busiest(&before, &now, 400, 1).len(), 1);
}

#[test]
fn the_busiest_on_the_card_are_only_programs_we_can_name() {
    let procs = [raw(10, "game", 0), raw(11, "game", 0), raw(30, "browser", 0)];
    let top = gpu_busiest(&[(10, 30.0), (11, 20.0), (30, 40.0), (99, 90.0), (31, 0.0)], &procs, 3);
    assert_eq!(top, vec![use_of("game", 50.0), use_of("browser", 40.0)]);
}

const I915_FDINFO: &str = "pos:\t0\nflags:\t02100002\nmnt_id:\t26\nino:\t361\ndrm-driver:\ti915\ndrm-pdev:\t0000:00:02.0\ndrm-client-id:\t42\ndrm-total-system0:\t8192 KiB\ndrm-engine-render:\t9000000000 ns\ndrm-engine-capacity-render:\t1\ndrm-engine-copy:\t100 ns\ndrm-engine-capacity-copy:\t1\ndrm-engine-video:\t4000000000 ns\ndrm-engine-capacity-video:\t2\ndrm-engine-video-enhance:\t0 ns\ndrm-engine-capacity-video-enhance:\t1\n";

const XE_FDINFO: &str = "pos:\t0\nflags:\t02100002\nmnt_id:\t26\nino:\t362\ndrm-driver:\txe\ndrm-client-id:\t7\ndrm-pdev:\t0000:00:02.0\ndrm-total-gtt:\t51200 KiB\ndrm-cycles-rcs:\t5000\ndrm-total-cycles-rcs:\t20000\ndrm-maxfreq-rcs:\t1300 MHz\ndrm-cycles-bcs:\t0\ndrm-total-cycles-bcs:\t20000\ndrm-cycles-vcs:\t10\n";

const AMDGPU_FDINFO: &str = "pos:\t0\nflags:\t02100002\nmnt_id:\t27\nino:\t450\ndrm-driver:\tamdgpu\ndrm-client-id:\t12\ndrm-pdev:\t0000:03:00.0\ndrm-memory-vram:\t1048576 KiB\ndrm-engine-gfx:\t5000000000 ns\ndrm-engine-compute:\t0 ns\ndrm-engine-dec:\t2000000000 ns\ndrm-engine-enc_1:\t0 ns\n";

fn engines(text: &str, edit: impl Fn(&str) -> String) -> DrmClient {
    parse_drm_fdinfo(&edit(text)).unwrap()
}

#[test]
fn fdinfo_of_each_driver_is_read() {
    let i915 = parse_drm_fdinfo(I915_FDINFO).unwrap();
    assert_eq!((i915.driver.as_str(), i915.pdev.as_deref(), i915.id), ("i915", Some("0000:00:02.0"), Some(42)));
    assert_eq!(i915.engines.len(), 4, "capacity lines are not engines");
    assert!(i915.engines.contains(&("video".into(), Busy::Ns { ns: 4_000_000_000, capacity: 2 })));
    assert!(i915.engines.contains(&("render".into(), Busy::Ns { ns: 9_000_000_000, capacity: 1 })));
    let xe = parse_drm_fdinfo(XE_FDINFO).unwrap();
    assert_eq!((xe.driver.as_str(), xe.id), ("xe", Some(7)));
    assert_eq!(xe.engines, vec![("rcs".into(), Busy::Cycles { busy: 5000, total: 20000 }), ("bcs".into(), Busy::Cycles { busy: 0, total: 20000 })], "vcs has no total, so no reading");
    let amd = parse_drm_fdinfo(AMDGPU_FDINFO).unwrap();
    assert_eq!((amd.driver.as_str(), amd.id, amd.engines.len()), ("amdgpu", Some(12), 4));
    // Not a graphics fd, or a unit we don't know
    assert!(parse_drm_fdinfo("pos:\t0\nflags:\t02\nmnt_id:\t9\n").is_none());
    assert!(parse_drm_fdinfo("").is_none());
    let odd = parse_drm_fdinfo("drm-driver: x\ndrm-engine-gfx: 5 ms\ndrm-engine-dec: oops ns\ndrm-client-id: nope\n").unwrap();
    assert!(odd.engines.is_empty() && odd.id.is_none() && odd.key().is_none());
}

#[test]
fn a_clients_share_is_its_busiest_engine_over_the_time_between_looks() {
    let second = 1_000_000_000;
    let before = parse_drm_fdinfo(I915_FDINFO).unwrap();
    // render busy 0.5 s of 1 s; video busy 1 s, but spread over two engines, so only half
    let after = engines(I915_FDINFO, |t| t.replace("9000000000", "9500000000").replace("4000000000", "5000000000"));
    assert_eq!(client_share(&before, &after, second), Some(50.0));
    let video = engines(I915_FDINFO, |t| t.replace("4000000000", "5200000000"));
    assert_eq!(client_share(&before, &video, second), Some(60.0), "the pair's 1.2 s is 60 percent, more than idle render");
    let busy = engines(I915_FDINFO, |t| t.replace("9000000000", "99000000000"));
    assert_eq!(client_share(&before, &busy, second), Some(100.0), "capped");
    assert_eq!(client_share(&before, &before, second), Some(0.0));
    assert_eq!(client_share(&before, &after, 0), None, "no time passed, nothing to compare");
}

#[test]
fn xe_clients_use_the_cycles_ratio_and_amdgpu_nanoseconds() {
    let before = parse_drm_fdinfo(XE_FDINFO).unwrap();
    let after = engines(XE_FDINFO, |t| t.replace("drm-cycles-rcs:\t5000", "drm-cycles-rcs:\t7000").replace("drm-total-cycles-rcs:\t20000", "drm-total-cycles-rcs:\t30000").replace("drm-cycles-bcs:\t0", "drm-cycles-bcs:\t1000"));
    // rcs: 2000 of 10000 cycles; bcs: its busy moved but its total didn't, so it has no rate
    let share = client_share(&before, &after, 5).unwrap();
    assert!((share - 20.0).abs() < 0.01, "{share}");
    let amd = parse_drm_fdinfo(AMDGPU_FDINFO).unwrap();
    let later = engines(AMDGPU_FDINFO, |t| t.replace("5000000000", "5600000000").replace("2000000000", "2900000000"));
    let share = client_share(&amd, &later, 1_000_000_000).unwrap();
    assert!((share - 90.0).abs() < 0.01, "decode at 90 percent beats graphics at 60: {share}");
}

#[test]
fn a_counter_that_went_backwards_is_no_reading() {
    let before = parse_drm_fdinfo(I915_FDINFO).unwrap();
    let reopened = engines(I915_FDINFO, |t| t.replace("9000000000", "10"));
    assert_eq!(client_share(&before, &reopened, 1_000_000_000), None);
    let xe = parse_drm_fdinfo(XE_FDINFO).unwrap();
    let back = engines(XE_FDINFO, |t| t.replace("drm-cycles-rcs:\t5000", "drm-cycles-rcs:\t1"));
    assert_eq!(client_share(&xe, &back, 1), None);
    // Nothing in common to compare is none too
    assert_eq!(client_share(&before, &xe, 1), None);
}

#[test]
fn a_shared_client_counts_once_under_the_first_process_and_clients_add_up_per_process() {
    let key = |c: &DrmClient| c.key().unwrap();
    let one = parse_drm_fdinfo(I915_FDINFO).unwrap();
    let two = engines(I915_FDINFO, |t| t.replace("client-id:\t42", "client-id:\t43"));
    let other_card = engines(I915_FDINFO, |t| t.replace("0000:00:02.0", "0000:03:00.0"));
    assert_ne!(key(&one), key(&two));
    assert_ne!(key(&one), key(&other_card), "the same id on another device is another client");
    let after = engines(I915_FDINFO, |t| t.replace("9000000000", "9300000000"));
    let after_two = engines(&I915_FDINFO.replace("client-id:\t42", "client-id:\t43"), |t| t.replace("9000000000", "9200000000"));
    let before: HashMap<ClientKey, DrmClient> = [(key(&one), one.clone()), (key(&two), two.clone())].into();
    // Two processes opened client 42 (a game and its helper); the first seen has it, so it is listed once
    let now: HashMap<ClientKey, (u32, DrmClient)> = [(key(&one), (100, after)), (key(&two), (100, after_two)), (key(&other_card), (200, other_card.clone()))].into();
    let shares = client_shares(&before, &now, 1_000_000_000);
    assert_eq!(shares.len(), 2, "the new client has no rate yet");
    assert!(shares.iter().all(|(pid, _)| *pid == 100));
    let load = gpu_load_from_clients(&shares, None).unwrap();
    assert_eq!(load.per_pid.len(), 1);
    assert!((load.per_pid[0].1 - 50.0).abs() < 0.01, "30 plus 20");
    assert!((load.total - 50.0).abs() < 0.01);
    // The driver's own total wins when it has one; a sum is capped
    assert_eq!(gpu_load_from_clients(&shares, Some(12.0)).unwrap().total, 12.0);
    assert_eq!(gpu_load_from_clients(&[(1, 80.0), (2, 70.0)], None).unwrap().total, 100.0);
    // Nothing to attribute: only the driver's total, or nothing
    assert_eq!(gpu_load_from_clients(&[], Some(33.0)), Some(GpuLoad { total: 33.0, per_pid: Vec::new() }));
    assert_eq!(gpu_load_from_clients(&[], None), None);
}

#[test]
fn a_game_on_the_shared_graphics_chip_is_named() {
    // Only integrated graphics: no total from the driver, so it is the sum of the clients
    let load = gpu_load_from_clients(&[(10, 90.0), (20, 4.0)], None).unwrap();
    let procs = [raw(10, "game", 0), raw(20, "telescope", 0)];
    let mut r = reading();
    r.gpu_percent = Some(load.total);
    r.own_gpu_percent = Some(3.0);
    r.gpu_busiest = gpu_busiest(&load.per_pid, &procs, 3);
    assert_eq!(r.gpu_busiest, vec![use_of("game", 90.0), use_of("telescope", 4.0)]);
    assert_eq!(likely_cause(&r), Some(Cause::GpuBusy { name: Some("game".into()) }));
}

fn reading() -> SystemReading {
    SystemReading { cpu_percent: Some(20.0), cores: 4, own_cpu_percent: Some(10.0), memory_total_mb: 8000, memory_available_mb: Some(4000), ..SystemReading::default() }
}

#[test]
fn another_program_hogging_the_processor_is_named() {
    let mut r = reading();
    r.busiest = vec![use_of("encoder", 55.0)];
    assert_eq!(likely_cause(&r), Some(Cause::OtherApp { name: "encoder".into(), cpu_percent: 55.0 }));
    // Under 40% alone isn't enough unless the whole computer is maxed out with Telescope a small part of it
    r.busiest = vec![use_of("encoder", 30.0)];
    assert_eq!(likely_cause(&r), None);
    r.cpu_percent = Some(92.0);
    r.own_cpu_percent = Some(20.0);
    assert_eq!(likely_cause(&r), Some(Cause::OtherApp { name: "encoder".into(), cpu_percent: 30.0 }));
    // Maxed out mostly by Telescope itself is not another program's doing
    r.own_cpu_percent = Some(60.0);
    assert_eq!(likely_cause(&r), Some(Cause::TelescopeItself));
}

#[test]
fn a_busy_card_is_blamed_on_the_program_using_it_most() {
    let mut r = reading();
    r.gpu_percent = Some(97.0);
    r.own_gpu_percent = Some(10.0);
    r.gpu_busiest = vec![use_of("game", 80.0)];
    assert_eq!(likely_cause(&r), Some(Cause::GpuBusy { name: Some("game".into()) }));
    // Busy, a small part ours, and nobody we may name: someone else's
    r.gpu_busiest.clear();
    assert_eq!(likely_cause(&r), Some(Cause::GpuBusy { name: None }));
    // Busy and we can't tell whose (Linux): no blame, since Telescope's own steps run on the card
    r.own_gpu_percent = None;
    assert_eq!(likely_cause(&r), None);
    // Mostly Telescope's own
    r.own_gpu_percent = Some(70.0);
    r.gpu_busiest = vec![use_of("game", 20.0)];
    assert_eq!(likely_cause(&r), Some(Cause::TelescopeItself));
    r.gpu_percent = Some(60.0);
    assert_eq!(likely_cause(&r), None, "not maxed out");
}

#[test]
fn battery_saver_low_memory_and_battery_are_hints_in_that_order() {
    let mut r = reading();
    assert_eq!(likely_cause(&r), None);
    r.on_battery = Some(true);
    assert_eq!(likely_cause(&r), Some(Cause::OnBattery));
    r.memory_available_mb = Some(700);
    assert_eq!(likely_cause(&r), Some(Cause::LowMemory), "under a tenth of 8000 MB");
    r.memory_available_mb = Some(400);
    r.memory_total_mb = 2000;
    assert_eq!(likely_cause(&r), Some(Cause::LowMemory), "under 500 MB");
    r.power_saver = Some(true);
    assert_eq!(likely_cause(&r), Some(Cause::BatterySaver));
    r.power_saver = Some(false);
    r.memory_available_mb = Some(1500);
    assert_eq!(likely_cause(&r), Some(Cause::OnBattery));
    r.on_battery = Some(false);
    assert_eq!(likely_cause(&r), None);
}

#[test]
fn a_computer_busy_mostly_with_telescope_says_so() {
    let mut r = reading();
    r.cpu_percent = Some(75.0);
    r.own_cpu_percent = Some(50.0);
    assert_eq!(likely_cause(&r), Some(Cause::TelescopeItself));
    r.own_cpu_percent = Some(20.0);
    assert_eq!(likely_cause(&r), None, "busy, but not mostly us");
    r.cpu_percent = Some(30.0);
    r.own_cpu_percent = Some(25.0);
    assert_eq!(likely_cause(&r), None, "not busy");
    // An unknown reading is never a cause
    assert_eq!(likely_cause(&SystemReading::default()), None);
}

#[test]
fn the_computer_can_be_read_here() {
    let mut reader = os::Reader::new();
    let first = reader.sample();
    assert!(first.memory_total_mb > 0, "memory total");
    assert!(first.memory_available_mb.is_some_and(|a| a <= first.memory_total_mb));
    assert!(first.cpu.is_some_and(|c| c.total > 0 && c.busy <= c.total), "processor times");
    assert!(first.own_ticks.is_some(), "this process's time");
    // The test's own parent (the test runner) is the user's process, so a reader that finds none is broken
    assert!(!first.procs.is_empty(), "the user's other processes");
    assert!(first.procs.iter().all(|p| p.pid != std::process::id() && !p.name.is_empty() && !p.name.contains(['\\', '/'])));
    assert!(first.procs.iter().all(|p| !p.name.to_lowercase().ends_with(".exe")), "names have no extension");
    // The whole read is the same shape with rates after a second look
    let mut monitor = Monitor::new();
    let one = monitor.read();
    assert_eq!((one.cpu_percent, one.own_cpu_percent, one.busiest.len()), (None, None, 0), "the first reading has no rates");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let two = monitor.read();
    assert!(two.cores >= 1 && two.memory_total_mb > 0);
    assert!(two.cpu_percent.is_some_and(|c| (0.0..=100.0).contains(&c)), "{:?}", two.cpu_percent);
    assert!(two.own_cpu_percent.is_some_and(|c| (0.0..=100.0).contains(&c)));
    assert!(two.busiest.len() <= 3 && two.busiest.windows(2).all(|w| w[0].percent >= w[1].percent));
    assert!(two.gpu_percent.is_none_or(|g| (0.0..=100.0).contains(&g)));
}

/// Prints one reading of this computer after a second, for a person to look at (run with --ignored --nocapture).
#[test]
#[ignore]
fn print_a_reading() {
    let mut monitor = Monitor::new();
    monitor.read();
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let reading = monitor.read();
    println!("{reading:#?}");
    println!("{:?}", likely_cause(&reading));
}

/// Prints the graphics part of one reading and how long its scan takes, with no program names (run with --ignored --nocapture).
#[test]
#[ignore]
#[cfg(target_os = "linux")]
fn print_the_graphics_part_and_scan_time() {
    let mut reader = os::Reader::new();
    let procs = reader.processes();
    reader.gpu(None);
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let load = reader.gpu(None);
    let names: HashMap<u32, usize> = procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
    let shares: Vec<String> = load.iter().flat_map(|l| &l.per_pid).map(|(pid, v)| format!("process {} at {v:.1}%", names.get(pid).map_or("(this one)".to_string(), |i| i.to_string()))).collect();
    println!("user processes: {}, total: {:?}, per process: {shares:?}", procs.len(), load.as_ref().map(|l| l.total));
    let runs = 50;
    let (mut listing, mut scan) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    for _ in 0..runs {
        let t = std::time::Instant::now();
        reader.processes();
        listing += t.elapsed();
        let t = std::time::Instant::now();
        reader.gpu(None);
        scan += t.elapsed();
    }
    println!("process listing {:?} per look, graphics scan {:?} per look", listing / runs, scan / runs);
}

#[test]
fn the_power_state_can_be_asked_here_without_waiting_long() {
    let began = std::time::Instant::now();
    // The first ask may only start a listener (Windows reports its power mode a moment after one registers)
    let _ = power_saver();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (first, second) = (power_saver(), power_saver());
    // Whatever this computer says, asking again agrees and doesn't hang on a bus or a daemon that isn't there
    assert_eq!(first, second);
    assert!(began.elapsed() < std::time::Duration::from_secs(3), "{:?}", began.elapsed());
    eprintln!("power_saver here: {first:?}");
}
