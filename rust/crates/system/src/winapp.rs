//! The Windows app's Start menu id and the warning for running from inside a zip (platform/windows.py).

use std::path::Path;

/// installer/Telescope.iss gives its shortcuts the same one.
pub const APP_USER_MODEL_ID: &str = "LunarKittyy.Telescope";

/// In the installed copy, takes the id its Start menu entry carries so Windows names notifications after it.
pub fn use_installed_app_id(app_dir: &Path) -> bool {
    crate::setup_move::has_uninstaller(app_dir) && set_app_id(APP_USER_MODEL_ID)
}

#[cfg(windows)]
fn set_app_id(id: &str) -> bool {
    #[link(name = "shell32")]
    extern "system" {
        fn SetCurrentProcessExplicitAppUserModelID(id: *const u16) -> i32;
    }
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: a NUL-terminated string that outlives the call
    unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) == 0 }
}

#[cfg(not(windows))]
fn set_app_id(_id: &str) -> bool {
    false
}

/// The temp folders zip tools run a file from: Explorer's Temp1_<name>.zip, 7-Zip's 7zO..., WinRAR's Rar$EX...
fn is_archive_temp(part: &str) -> bool {
    let name = part.to_lowercase();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    // temp\d+_.*\.zip$
    let explorer = name.strip_prefix("temp").is_some_and(|rest| {
        let after = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        after.len() < rest.len() && after.strip_prefix('_').is_some_and(|tail| tail.ends_with(".zip"))
    });
    // 7zo\w+$
    let seven_zip = name.strip_prefix("7zo").is_some_and(|rest| !rest.is_empty() && rest.chars().all(word));
    // rar\$ex\w*\..*$
    let winrar = name.strip_prefix("rar$ex").is_some_and(|rest| rest.trim_start_matches(word).starts_with('.'));
    explorer || seven_zip || winrar
}

/// Whether Telescope was opened from inside a zip, so it runs from a copy in the temp folder that goes away.
pub fn running_from_archive(app_dir: &Path, temp_dir: &Path) -> bool {
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    resolve(app_dir).strip_prefix(resolve(temp_dir)).is_ok_and(|rel| rel.components().any(|c| is_archive_temp(&c.as_os_str().to_string_lossy())))
}

/// Says so in a message box, since there is no window yet to say it in.
#[cfg(windows)]
pub fn warn_running_from_archive() {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(window: *mut std::ffi::c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    // The launcher's words in the person's language (the language is loaded by now)
    let (title, text) = crate::splash::extract_first();
    let (text, caption) = (wide(&text), wide(&title));
    // SAFETY: both strings are NUL-terminated and outlive the call; 0x30 is MB_ICONWARNING
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), 0x30) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::scratch_dir;

    // test_platform_windows.py::test_running_from_archive_spots_zip_tools_temp_folders
    #[test]
    fn running_from_archive_spots_zip_tools_temp_folders() {
        for (inside, archived) in [
            ("Temp1_Telescope-windows-portable.zip/Telescope", true), // Explorer's Run from the zip
            ("7zO4A8C1B2E", true),                                    // 7-Zip
            ("Rar$EXa1234.5678/Telescope", true),                     // WinRAR
            ("Telescope", false),                                     // extracted into temp on purpose
            ("_MEI12345", false),                                     // PyInstaller's own unpack folder
        ] {
            let temp = scratch_dir("archive");
            let app = temp.join(inside);
            std::fs::create_dir_all(&app).unwrap();
            assert_eq!(running_from_archive(&app, &temp), archived, "{inside}");
        }
    }

    // test_platform_windows.py::test_running_from_archive_ignores_folders_outside_temp
    #[test]
    fn running_from_archive_ignores_folders_outside_temp() {
        let root = scratch_dir("archive");
        let app = root.join("Downloads").join("Temp1_Telescope-windows-portable.zip");
        std::fs::create_dir_all(&app).unwrap();
        assert!(!running_from_archive(&app, &root.join("Temp")));
    }

    #[test]
    fn the_archive_names_need_all_of_their_pattern() {
        for name in ["temp1_x.zip", "TEMP22_.ZIP", "7zOabc", "rar$ex.1", "Rar$EXa1.b"] {
            assert!(is_archive_temp(name), "{name}");
        }
        for name in ["temp_x.zip", "temp1x.zip", "temp1_x.rar", "7zo", "7zo-1", "rar$exa-b", "rar$exa"] {
            assert!(!is_archive_temp(name), "{name}");
        }
    }

    // test_platform_windows.py::test_only_the_installed_copy_takes_the_start_menu_entrys_app_id
    #[test]
    fn only_the_installed_copy_takes_the_start_menu_entrys_app_id() {
        // Unzipped: no uninstaller, so no shortcut with the id
        assert!(!use_installed_app_id(&scratch_dir("appid")));
    }
}
