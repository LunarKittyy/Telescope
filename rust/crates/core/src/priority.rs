//! Lets a thread that isn't on the frame path ask the system to run it behind the frames and behind other programs. Only a thread says this about itself, and only for work whose answer may arrive a frame late.

use std::sync::OnceLock;

/// How a background thread is lowered. Measured with `TELESCOPE_BACKGROUND_PRIORITY` (a developer's switch, not for people).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    #[cfg(unix)]
    Nice(i32),
    #[cfg(unix)]
    Idle,
    #[cfg(windows)]
    Below { eco: bool },
    #[cfg(windows)]
    Lowest { eco: bool },
    #[cfg(windows)]
    Eco,
}

fn parse(text: &str) -> Option<Mode> {
    let text = text.trim().to_ascii_lowercase();
    if text == "normal" || text == "off" {
        return Some(Mode::Normal);
    }
    #[cfg(unix)]
    {
        if text == "idle" {
            return Some(Mode::Idle);
        }
        if let Some(n) = text.strip_prefix("nice").and_then(|n| n.parse::<i32>().ok()) {
            return Some(Mode::Nice(n.clamp(0, 19)));
        }
    }
    #[cfg(windows)]
    {
        return match text.as_str() {
            "below" => Some(Mode::Below { eco: false }),
            "below+eco" => Some(Mode::Below { eco: true }),
            "lowest" => Some(Mode::Lowest { eco: false }),
            "lowest+eco" => Some(Mode::Lowest { eco: true }),
            "eco" => Some(Mode::Eco),
            _ => None,
        };
    }
    #[allow(unreachable_code)]
    None
}

fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| std::env::var("TELESCOPE_BACKGROUND_PRIORITY").ok().and_then(|t| parse(&t)).unwrap_or_else(default_mode))
}

#[cfg(unix)]
fn default_mode() -> Mode {
    // Measured best of nice 10, nice 19 and idle: the frame path's delay fell by a third and fewer masks were lost than at nice 19
    Mode::Idle
}

#[cfg(windows)]
fn default_mode() -> Mode {
    Mode::Below { eco: true }
}

#[cfg(not(any(unix, windows)))]
fn default_mode() -> Mode {
    Mode::Normal
}

/// The calling thread runs behind the frame path from now on. Failing to is harmless: the thread just keeps its priority.
pub fn behind_the_frames() {
    apply(mode());
}

/// The same for the app's threads, but never in a test: a test run is many tests at once, and a lowered thread there can wait seconds for its turn.
pub fn behind_the_frames_unless_testing() {
    if !cfg!(test) && std::env::var_os("NEXTEST").is_none() {
        behind_the_frames();
    }
}

#[cfg(unix)]
fn apply(mode: Mode) {
    match mode {
        Mode::Normal => {}
        Mode::Nice(n) => {
            // SAFETY: setpriority with PRIO_PROCESS and a thread id changes that one thread's nice value on Linux
            #[cfg(target_os = "linux")]
            unsafe {
                libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, n);
            }
            #[cfg(not(target_os = "linux"))]
            let _ = n;
        }
        Mode::Idle => {
            #[cfg(target_os = "linux")]
            {
                let param = libc::sched_param { sched_priority: 0 };
                // SAFETY: pid 0 is the calling thread on Linux, and the param is a valid zeroed priority
                unsafe {
                    libc::sched_setscheduler(0, libc::SCHED_IDLE, &param);
                }
            }
        }
    }
}

#[cfg(windows)]
fn apply(mode: Mode) {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadInformation, SetThreadPriority, ThreadPowerThrottling, THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE,
        THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_LOWEST,
    };
    let (priority, eco) = match mode {
        Mode::Normal => return,
        Mode::Below { eco } => (Some(THREAD_PRIORITY_BELOW_NORMAL), eco),
        Mode::Lowest { eco } => (Some(THREAD_PRIORITY_LOWEST), eco),
        Mode::Eco => (None, true),
    };
    // SAFETY: the pseudo handle of the calling thread needs no closing, and the state struct is what ThreadPowerThrottling takes
    unsafe {
        let me = GetCurrentThread();
        if let Some(priority) = priority {
            let _ = SetThreadPriority(me, priority);
        }
        if eco {
            // EcoQoS: the system may run this thread slowly and on the efficient cores, and doesn't count it as work to hurry
            let state = THREAD_POWER_THROTTLING_STATE { Version: THREAD_POWER_THROTTLING_CURRENT_VERSION, ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED, StateMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED };
            let _ = SetThreadInformation(me, ThreadPowerThrottling, (&raw const state).cast(), size_of::<THREAD_POWER_THROTTLING_STATE>() as u32);
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn apply(_mode: Mode) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_developers_switch_names_a_mode_and_anything_else_is_ignored() {
        assert_eq!(parse("normal"), Some(Mode::Normal));
        assert_eq!(parse(" OFF "), Some(Mode::Normal));
        assert_eq!(parse("sideways"), None);
        #[cfg(unix)]
        {
            assert_eq!(parse("nice19"), Some(Mode::Nice(19)));
            assert_eq!(parse("nice99"), Some(Mode::Nice(19)));
            assert_eq!(parse("idle"), Some(Mode::Idle));
        }
        #[cfg(windows)]
        {
            assert_eq!(parse("below+eco"), Some(Mode::Below { eco: true }));
            assert_eq!(parse("eco"), Some(Mode::Eco));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_thread_that_lowers_itself_leaves_the_rest_alone() {
        let nice = |tid: libc::pid_t| unsafe { libc::getpriority(libc::PRIO_PROCESS, tid as libc::id_t) };
        let before = nice(0);
        let lowered = std::thread::spawn(|| {
            apply(Mode::Nice(7));
            nice(0)
        })
        .join()
        .unwrap();
        assert_eq!(lowered, before.max(7));
        assert_eq!(nice(0), before, "this thread's priority didn't move with it");
    }
}
