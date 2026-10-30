// A lock that's held by something that never answers (test_app.py's stale listener tests): the start says so rather than hang or
// quit without a word, and the connection it tried is closed each time. Alone in its binary so no other test opens sockets meanwhile.
#![cfg(target_os = "linux")]

use std::time::Duration;
use telescope_system::single_instance::{acquire, notice, Acquired, Address};

/// An abstract Unix socket that's bound but not listening: its name is taken and connecting to it is refused.
fn bound_but_not_listening(name: &str) -> i32 {
    // SAFETY: plain socket calls on a zeroed sockaddr_un that's filled in before bind
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        assert!(fd >= 0);
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (i, b) in name.bytes().enumerate() {
            addr.sun_path[i + 1] = b as libc::c_char;
        }
        let len = (std::mem::size_of::<libc::sa_family_t>() + 1 + name.len()) as libc::socklen_t;
        assert_eq!(libc::bind(fd, &addr as *const libc::sockaddr_un as *const libc::sockaddr, len), 0);
        fd
    }
}

fn open_sockets() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().flatten()
        .filter(|e| std::fs::read_link(e.path()).is_ok_and(|t| t.to_string_lossy().starts_with("socket:")))
        .count()
}

// test_acquire_single_instance_tolerates_stale_listener, test_acquire_single_instance_closes_failed_notification_socket
#[test]
fn a_stale_listener_is_told_so_and_leaves_no_socket_open() {
    let name = format!("telescope-test-stale-{}", std::process::id());
    let holder = bound_but_not_listening(&name);
    let address = Address::Abstract(name);
    let before = open_sockets();
    for _ in 0..20 {
        let result = acquire(&address, Duration::ZERO);
        assert!(matches!(result, Acquired::Taken));
        assert!(notice(&result, &address).unwrap().1.contains("a local socket"));
    }
    assert_eq!(open_sockets(), before);
    // SAFETY: closing the descriptor made above
    unsafe { libc::close(holder) };
    assert!(matches!(acquire(&address, Duration::ZERO), Acquired::Lock(_)));
}
