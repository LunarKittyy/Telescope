//! The Windows registry through advapi32 (winreg's way) rather than reg.exe, whose text comes out in a code page.

/// The text of a registry value's data (UTF-16, as winreg gets it) up to its first NUL; None unless it is a REG_SZ or REG_EXPAND_SZ.
pub fn registry_text(kind: u32, data: &[u16]) -> Option<String> {
    matches!(kind, 1 | 2).then(|| String::from_utf16_lossy(data.split(|&unit| unit == 0).next().unwrap_or_default()))
}

/// What a REG_SZ stores for text: its UTF-16 and a closing NUL, as the bytes RegSetValueExW takes.
pub fn registry_data(text: &str) -> Vec<u8> {
    text.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect()
}

#[cfg(windows)]
pub use machine::*;

#[cfg(windows)]
mod machine {
    use super::{registry_data, registry_text};
    use std::ffi::c_void;
    use std::io;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(key: *mut c_void, sub: *const u16, options: u32, access: u32, out: *mut *mut c_void) -> i32;
        fn RegCreateKeyExW(key: *mut c_void, sub: *const u16, reserved: u32, class: *const u16, options: u32, access: u32, security: *const c_void, out: *mut *mut c_void, disposition: *mut u32) -> i32;
        fn RegQueryValueExW(key: *mut c_void, name: *const u16, reserved: *mut u32, kind: *mut u32, data: *mut u8, len: *mut u32) -> i32;
        fn RegSetValueExW(key: *mut c_void, name: *const u16, reserved: u32, kind: u32, data: *const u8, len: u32) -> i32;
        fn RegDeleteValueW(key: *mut c_void, name: *const u16) -> i32;
        fn RegCloseKey(key: *mut c_void) -> i32;
    }

    const KEY_READ: u32 = 0x20019;
    const KEY_SET_VALUE: u32 = 0x0002;
    const REG_SZ: u32 = 1;
    const REG_DWORD: u32 = 4;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PATH_NOT_FOUND: i32 = 3;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn root(hive: &str) -> io::Result<*mut c_void> {
        match hive {
            "HKCR" => Ok(0x8000_0000_usize as *mut c_void),
            "HKCU" => Ok(0x8000_0001_usize as *mut c_void),
            _ => Err(io::ErrorKind::InvalidInput.into()),
        }
    }

    fn checked(code: i32) -> io::Result<()> {
        if code == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(code)) }
    }

    fn missing(e: &io::Error) -> bool {
        matches!(e.raw_os_error(), Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND))
    }

    /// An open key, closed when it goes.
    struct Key(*mut c_void);

    impl Key {
        fn open(hive: &str, key: &str, access: u32) -> io::Result<Key> {
            let (root, key) = (root(hive)?, wide(key));
            let mut open = std::ptr::null_mut();
            // SAFETY: a NUL-terminated string that outlives the call, and somewhere to put the handle
            checked(unsafe { RegOpenKeyExW(root, key.as_ptr(), 0, access, &mut open) })?;
            Ok(Key(open))
        }

        /// Makes the key if it isn't there (winreg.CreateKey).
        fn create(hive: &str, key: &str, access: u32) -> io::Result<Key> {
            let (root, key) = (root(hive)?, wide(key));
            let mut open = std::ptr::null_mut();
            // SAFETY: as open; the class, security and disposition may all be null
            checked(unsafe {
                RegCreateKeyExW(root, key.as_ptr(), 0, std::ptr::null(), 0, access, std::ptr::null(), &mut open, std::ptr::null_mut())
            })?;
            Ok(Key(open))
        }

        fn set(&self, name: &str, kind: u32, data: &[u8]) -> io::Result<()> {
            let name = wide(name);
            let len = u32::try_from(data.len()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            // SAFETY: the name is NUL-terminated and data is len bytes, both outlive the call
            checked(unsafe { RegSetValueExW(self.0, name.as_ptr(), 0, kind, data.as_ptr(), len) })
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: a handle this type opened, closed once
            unsafe { RegCloseKey(self.0) };
        }
    }

    /// A value's kind and UTF-16 data; None if it or its key isn't there.
    pub fn read_value(hive: &str, key: &str, name: &str) -> io::Result<Option<(u32, Vec<u16>)>> {
        let open = match Key::open(hive, key, KEY_READ) {
            Err(e) if missing(&e) => return Ok(None),
            other => other?,
        };
        let name = wide(name);
        let (mut kind, mut len) = (0u32, 0u32);
        // SAFETY: the name is NUL-terminated; this first query only asks the size, the second reads into a buffer that big
        unsafe {
            match checked(RegQueryValueExW(open.0, name.as_ptr(), std::ptr::null_mut(), &mut kind, std::ptr::null_mut(), &mut len)) {
                Err(e) if missing(&e) => return Ok(None),
                other => other?,
            }
            let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
            let mut size = (buf.len() * 2) as u32;
            checked(RegQueryValueExW(open.0, name.as_ptr(), std::ptr::null_mut(), &mut kind, buf.as_mut_ptr().cast(), &mut size))?;
            buf.truncate(size as usize / 2);
            Ok(Some((kind, buf)))
        }
    }

    /// A string value under hive ("HKCU" or "HKCR") at key, "" for the key's default value; None if it isn't there or isn't text.
    pub fn read_text(hive: &str, key: &str, name: &str) -> Option<String> {
        let (kind, data) = read_value(hive, key, name).ok().flatten()?;
        registry_text(kind, &data)
    }

    /// Sets a string value, making the key if it isn't there.
    pub fn set_text(hive: &str, key: &str, name: &str, text: &str) -> io::Result<()> {
        Key::create(hive, key, KEY_SET_VALUE)?.set(name, REG_SZ, &registry_data(text))
    }

    /// Sets a number value in a key that's there.
    pub fn set_number(hive: &str, key: &str, name: &str, value: u32) -> io::Result<()> {
        Key::open(hive, key, KEY_SET_VALUE)?.set(name, REG_DWORD, &value.to_le_bytes())
    }

    /// Takes a value away; fine if it or its key wasn't there.
    pub fn delete_value(hive: &str, key: &str, name: &str) -> io::Result<()> {
        let open = match Key::open(hive, key, KEY_SET_VALUE) {
            Err(e) if missing(&e) => return Ok(()),
            other => other?,
        };
        let name = wide(name);
        // SAFETY: a NUL-terminated string that outlives the call
        match checked(unsafe { RegDeleteValueW(open.0, name.as_ptr()) }) {
            Err(e) if missing(&e) => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_value_keeps_its_characters_both_ways() {
        let line = "\"C:\\Users\\Åsa\\日本語\\TelescopeDesktop.exe\" --minimized";
        let bytes = registry_data(line);
        assert_eq!(bytes.len(), (line.encode_utf16().count() + 1) * 2, "with the closing NUL, counted in bytes");
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(units.last(), Some(&0));
        assert_eq!(registry_text(1, &units).as_deref(), Some(line));
        assert_eq!(registry_text(4, &units), None);
    }
}
