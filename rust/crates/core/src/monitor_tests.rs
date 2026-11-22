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
