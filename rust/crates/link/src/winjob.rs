//! Windows job object that kills its processes when Telescope exits, however it exits (platform/winjob.py).
//! A job with KILL_ON_JOB_CLOSE stands in for a parent-child lifetime; its handle lives in this process, so Windows
//! kills everything in it when Telescope quits or crashes. Only processes added with kill_with_us() are in it.

use std::process::Child;

/// Puts a child in the job, so it dies when Telescope does. False if Windows refused, or off Windows.
pub fn kill_with_us(child: &Child) -> bool {
    match imp::assign(child) {
        Ok(()) => true,
        Err(why) => {
            log::warn!("Couldn't tie adb's server to Telescope's lifetime: {why}");
            false
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::sync::Mutex;

    type Handle = *mut c_void;

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *mut c_void, length: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    // The job's handle as an address, so the static is Send; it's never closed, Windows closes it at exit
    static JOB: Mutex<Option<usize>> = Mutex::new(None);

    fn create() -> Result<usize, String> {
        // SAFETY: plain Win32 calls with a zeroed, correctly sized struct; the handle is checked before use
        unsafe {
            let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
            if job.is_null() {
                return Err(format!("CreateJobObject failed: {}", std::io::Error::last_os_error()));
            }
            let mut info = ExtendedLimits::default();
            info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = std::mem::size_of::<ExtendedLimits>() as u32;
            if SetInformationJobObject(job, JOB_OBJECT_EXTENDED_LIMIT_INFORMATION, &mut info as *mut _ as *mut c_void, size) == 0 {
                let err = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(format!("SetInformationJobObject failed: {err}"));
            }
            Ok(job as usize)
        }
    }

    pub fn assign(child: &Child) -> Result<(), String> {
        let mut held = JOB.lock().unwrap_or_else(|e| e.into_inner());
        let job = match *held {
            Some(job) => job,
            None => *held.insert(create()?),
        };
        // SAFETY: both handles are open: the job is never closed and child owns its process handle
        if unsafe { AssignProcessToJobObject(job as Handle, child.as_raw_handle() as Handle) } == 0 {
            return Err(format!("AssignProcessToJobObject failed: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use std::process::Child;

    pub fn assign(_child: &Child) -> Result<(), String> {
        Err("job objects are Windows only".into())
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn the_windows_job_fails_softly_elsewhere() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        assert!(!kill_with_us(&child));
        let _ = child.wait();
    }
}
