//! Open Telescope at sign-in: an XDG autostart entry on Linux, the per-user Run key on Windows; also the Linux app-menu entry and the hidden one the portal needs (platform/autostart.py).

use std::path::{Path, PathBuf};

use telescope_core::i18n::tr;

/// The menu entry's file name, its icon's name, and the window's app id.
pub const APP_ID: &str = "telescope";
pub const MINIMIZED: &str = "--minimized";
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE: &str = "Telescope";

/// The per-user Run key's Telescope value, behind a trait so tests can fake the registry.
pub trait RunKey {
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, line: &str) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;
}

/// Where opening at sign-in is kept on this computer.
pub enum Place<'a> {
    /// The config home that holds autostart/telescope.desktop.
    Xdg(PathBuf),
    Registry(&'a dyn RunKey),
}

/// How to start this copy, minimized to the tray unless minimized is false.
pub fn launch_command(directory: &Path, minimized: bool) -> Vec<String> {
    let start_sh = directory.join("start.sh");
    let program = if !cfg!(windows) && start_sh.exists() {
        start_sh
    } else {
        std::env::current_exe().map(|p| p.canonicalize().unwrap_or(p)).unwrap_or_else(|_| directory.join("telescope-rs"))
    };
    let mut argv = vec![program.to_string_lossy().into_owned()];
    if minimized {
        argv.push(MINIMIZED.into());
    }
    argv
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}

fn data_home(data_home: Option<&Path>) -> PathBuf {
    data_home.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".local").join("share"))
    })
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn desktop_file(config_home: &Path) -> PathBuf {
    config_home.join("autostart").join("telescope.desktop")
}

pub fn menu_file(data: Option<&Path>) -> PathBuf {
    data_home(data).join("applications").join(format!("{APP_ID}.desktop"))
}

pub fn icon_file(data: Option<&Path>) -> PathBuf {
    data_home(data).join("icons").join("hicolor").join("256x256").join("apps").join(format!("{APP_ID}.png"))
}

/// Quotes one argument for a desktop entry's Exec key (the spec's rules, not a shell's).
pub fn exec_arg(arg: &str) -> String {
    if !arg.chars().any(|c| " \t\n\"'\\><~|&;$*?#()`".contains(c)) {
        return arg.to_string();
    }
    let escaped: String = arg.chars().map(|c| if "\"`$\\".contains(c) { format!("\\{c}") } else { c.to_string() }).collect();
    format!("\"{escaped}\"")
}

pub fn desktop_entry(command: &[String], menu: bool) -> String {
    let exec_line = command.iter().map(|a| exec_arg(a).replace('%', "%%")).collect::<Vec<_>>().join(" ");
    let kind = if menu { "Categories=AudioVideo;Video;\nKeywords=webcam;camera;phone;android;\n" } else { "X-GNOME-Autostart-enabled=true\n" };
    format!(
        "[Desktop Entry]\nType=Application\nName=Telescope\nGenericName=Phone webcam\nComment=Phone camera as a webcam\n\
         Exec={exec_line}\nIcon={APP_ID}\nStartupWMClass={APP_ID}\nTerminal=false\n{kind}"
    )
}

/// Linux: points the app menu's entry at this copy, writing it only when it changed; save_icon writes the icon when it's missing. Returns whether it wrote.
pub fn update_menu_entry(save_icon: impl FnOnce(&Path) -> bool, command: &[String], data: Option<&Path>) -> bool {
    let text = desktop_entry(command, true);
    let path = menu_file(data);
    let icon = icon_file(data);
    let write = || -> std::io::Result<bool> {
        if !icon.exists() {
            std::fs::create_dir_all(icon.parent().unwrap_or(Path::new(".")))?;
            save_icon(&icon);
        }
        if std::fs::read_to_string(&path).is_ok_and(|t| t == text) {
            return Ok(false);
        }
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(&path, &text)?;
        Ok(true)
    };
    write().unwrap_or_else(|e| {
        log::info!("Couldn't write the app menu entry: {e}");
        false
    })
}

/// The app icon (create_app_icon): an accent-coloured disc with a dark centre, as straight RGBA rows, edges smoothed by 4x4 sampling.
pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    const ACCENT: [f64; 3] = [0xaa as f64, 0x9c as f64, 0xf5 as f64];
    const BG: [f64; 3] = [0x0f as f64, 0x12 as f64, 0x16 as f64];
    let s = size as f64;
    let margin = s / 22.0;
    let (outer_r, inner_r, c) = ((s - 2.0 * margin) / 2.0, s * 8.0 / 22.0 / 2.0, s / 2.0);
    let mut raw = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (mut accent, mut dark) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let (px, py) = (x as f64 + (sx as f64 + 0.5) / 4.0 - c, y as f64 + (sy as f64 + 0.5) / 4.0 - c);
                    let d = (px * px + py * py).sqrt();
                    if d <= inner_r {
                        dark += 1;
                    } else if d <= outer_r {
                        accent += 1;
                    }
                }
            }
            let covered = accent + dark;
            if covered == 0 {
                raw.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let mix = |i: usize| ((ACCENT[i] * accent as f64 + BG[i] * dark as f64) / covered as f64).round() as u8;
            raw.extend_from_slice(&[mix(0), mix(1), mix(2), (covered as f64 * 255.0 / 16.0).round() as u8]);
        }
    }
    raw
}

/// app_icon_rgba as PNG bytes.
pub fn app_icon_png(size: u32) -> Vec<u8> {
    use std::io::Write;
    // Each row starts with the filter byte 0 (none)
    let raw: Vec<u8> = app_icon_rgba(size).chunks((size * 4) as usize).flat_map(|row| std::iter::once(0u8).chain(row.iter().copied())).collect();
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = zlib.write_all(&raw);
    let data = zlib.finish().unwrap_or_default();
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc = 0xffff_ffffu32;
        for &b in &out[start..] {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&size.to_be_bytes());
    header.extend_from_slice(&size.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 bit RGBA
    chunk(&mut png, b"IHDR", &header);
    chunk(&mut png, b"IDAT", &data);
    chunk(&mut png, b"IEND", &[]);
    png
}

/// Linux: update_menu_entry with the app icon (main.py's call). Returns whether the entry was written.
pub fn refresh_menu_entry(command: &[String], data: Option<&Path>) -> bool {
    cfg!(target_os = "linux") && update_menu_entry(|path| std::fs::write(path, app_icon_png(256)).is_ok(), command, data)
}

fn has_entry(app_id: &str, data: Option<&Path>) -> bool {
    let system = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    std::iter::once(data_home(data))
        .chain(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from))
        .any(|d| d.join("applications").join(format!("{app_id}.desktop")).exists())
}

/// Linux: makes sure there's a menu entry named app_id, which the portal wants before it lets Telescope register global shortcuts; a copy without its own gets a hidden one.
pub fn ensure_portal_entry(app_id: &str, command: &[String], data: Option<&Path>) -> bool {
    if has_entry(app_id, data) {
        return true;
    }
    let text = desktop_entry(command, true) + "NoDisplay=true\n";
    let path = data_home(data).join("applications").join(format!("{app_id}.desktop"));
    let written = std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).and_then(|_| std::fs::write(&path, text));
    if let Err(e) = written {
        log::info!("Couldn't write a menu entry for the desktop portal: {e}");
        return false;
    }
    true
}

/// The Run key's value: arguments with a space in quotes.
pub fn run_line(command: &[String]) -> String {
    command.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" ")
}

pub fn is_enabled(place: &Place) -> bool {
    match place {
        Place::Xdg(home) => desktop_file(home).exists(),
        Place::Registry(key) => matches!(key.get(), Ok(Some(_))),
    }
}

pub fn enable(command: &[String], place: &Place) -> Result<(), String> {
    let done = match place {
        Place::Xdg(home) => {
            let path = desktop_file(home);
            std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
                .and_then(|_| std::fs::write(&path, desktop_entry(command, false)))
                .map_err(|e| e.to_string())
        }
        Place::Registry(key) => key.set(&run_line(command)),
    };
    done.map_err(|e| tr("Couldn't set Telescope to open at sign-in: {error}").replace("{error}", &e))
}

/// If opening at sign-in is on but points at another copy (the folder moved), points it at this one.
pub fn refresh(command: &[String], place: &Place) -> bool {
    let current = match place {
        Place::Xdg(home) => match std::fs::read_to_string(desktop_file(home)) {
            Ok(text) => text == desktop_entry(command, false),
            Err(_) => return false,
        },
        Place::Registry(key) => match key.get() {
            Ok(Some(line)) => line == run_line(command),
            _ => return false,
        },
    };
    !current && enable(command, place).is_ok()
}

pub fn disable(place: &Place) -> Result<(), String> {
    let done = match place {
        Place::Xdg(home) => match std::fs::remove_file(desktop_file(home)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        },
        Place::Registry(key) => key.delete(),
    };
    done.map_err(|e| tr("Couldn't stop Telescope opening at sign-in: {error}").replace("{error}", &e))
}

/// The real Run key, through advapi32 like winreg, so a path outside the code page survives.
pub struct Registry;

#[cfg(windows)]
impl RunKey for Registry {
    fn get(&self) -> Result<Option<String>, String> {
        // A value that isn't text still counts as there, and refresh() puts a good one over it
        let value = crate::registry::read_value("HKCU", RUN_KEY, VALUE).map_err(|e| e.to_string())?;
        Ok(value.map(|(kind, data)| crate::registry::registry_text(kind, &data).unwrap_or_default()))
    }

    fn set(&self, line: &str) -> Result<(), String> {
        crate::registry::set_text("HKCU", RUN_KEY, VALUE, line).map_err(|e| e.to_string())
    }

    fn delete(&self) -> Result<(), String> {
        crate::registry::delete_value("HKCU", RUN_KEY, VALUE).map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
impl RunKey for Registry {
    fn get(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn set(&self, _line: &str) -> Result<(), String> {
        Err("There's no registry here".into())
    }

    fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// This computer's place: the Run key on Windows, $XDG_CONFIG_HOME/autostart elsewhere.
pub fn here() -> Place<'static> {
    if cfg!(windows) { Place::Registry(&Registry) } else { Place::Xdg(config_home()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("telescope-autostart-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn linux_desktop_entry_round_trip() {
        let dir = tmp("round");
        let place = Place::Xdg(dir.clone());
        let command = argv(&["/home/someone/My Apps/Telescope/start.sh", "--minimized"]);
        assert!(!is_enabled(&place));
        assert_eq!(enable(&command, &place), Ok(()));
        let text = std::fs::read_to_string(dir.join("autostart/telescope.desktop")).unwrap();
        assert!(text.contains("Exec=\"/home/someone/My Apps/Telescope/start.sh\" --minimized\n"));
        assert!(is_enabled(&place));
        assert_eq!(disable(&place), Ok(()));
        assert!(!is_enabled(&place));
        assert_eq!(disable(&place), Ok(()));
    }

    #[test]
    fn the_sign_in_entry_follows_this_copy_when_it_is_on() {
        let dir = tmp("follow");
        let place = Place::Xdg(dir.clone());
        assert!(!refresh(&argv(&["/new/start.sh", "--minimized"]), &place));
        assert!(!is_enabled(&place));
        enable(&argv(&["/old/start.sh", "--minimized"]), &place).unwrap();
        assert!(refresh(&argv(&["/new/start.sh", "--minimized"]), &place));
        assert!(std::fs::read_to_string(dir.join("autostart/telescope.desktop")).unwrap().contains("Exec=/new/start.sh --minimized\n"));
        assert!(!refresh(&argv(&["/new/start.sh", "--minimized"]), &place));
    }

    #[test]
    fn exec_quoting_follows_the_desktop_entry_rules() {
        assert_eq!(exec_arg("/usr/bin/python3"), "/usr/bin/python3");
        assert_eq!(exec_arg("/a b/\"x\"$y"), "\"/a b/\\\"x\\\"\\$y\"");
        assert!(desktop_entry(&argv(&["/x/100%", "--minimized"]), false).contains("100%%"));
    }

    #[test]
    fn launch_command_prefers_start_sh_on_linux() {
        let dir = tmp("launch");
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap().to_string_lossy().into_owned();
        assert_eq!(launch_command(&dir, true), vec![exe.clone(), MINIMIZED.to_string()]);
        std::fs::write(dir.join("start.sh"), "#!/bin/sh\n").unwrap();
        let start = dir.join("start.sh").to_string_lossy().into_owned();
        assert_eq!(launch_command(&dir, true), vec![start.clone(), MINIMIZED.to_string()]);
        assert_eq!(launch_command(&dir, false), vec![start]);
    }

    #[test]
    fn menu_entry_follows_this_copy_and_only_writes_on_change() {
        let dir = tmp("menu");
        let icons = RefCell::new(Vec::new());
        let save_icon = |p: &Path| {
            icons.borrow_mut().push(p.to_path_buf());
            std::fs::write(p, b"png").is_ok()
        };
        let command = argv(&["/home/someone/My Apps/Telescope/start.sh"]);
        assert!(update_menu_entry(save_icon, &command, Some(&dir)));
        let text = std::fs::read_to_string(dir.join("applications/telescope.desktop")).unwrap();
        assert!(text.contains("Exec=\"/home/someone/My Apps/Telescope/start.sh\"\n"));
        assert!(text.contains("Icon=telescope\n") && text.contains("Categories=") && !text.contains("Autostart"));
        assert_eq!(*icons.borrow(), vec![dir.join("icons/hicolor/256x256/apps/telescope.png")]);
        assert!(!update_menu_entry(save_icon, &command, Some(&dir)));
        assert!(update_menu_entry(save_icon, &argv(&["/elsewhere/start.sh"]), Some(&dir)));
        assert!(std::fs::read_to_string(dir.join("applications/telescope.desktop")).unwrap().contains("Exec=/elsewhere/start.sh\n"));
        assert_eq!(icons.borrow().len(), 1);
    }

    #[test]
    fn the_app_icon_is_an_accent_disc_with_a_dark_centre() {
        use std::io::Read;
        let png = app_icon_png(256);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 256);
        let len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&png[41..41 + len]).read_to_end(&mut raw).unwrap();
        let at = |x: usize, y: usize| -> [u8; 4] { raw[y * (256 * 4 + 1) + 1 + x * 4..][..4].try_into().unwrap() };
        assert_eq!(at(128, 128), [0x0f, 0x12, 0x16, 255]);
        assert_eq!(at(128, 20), [0xaa, 0x9c, 0xf5, 255]);
        assert_eq!(at(0, 0)[3], 0);
    }

    #[test]
    fn menu_entry_trouble_is_not_fatal() {
        let dir = tmp("trouble");
        std::fs::write(dir.join("applications"), "a file where the folder should be").unwrap();
        assert!(!update_menu_entry(|_| true, &argv(&["/x/start.sh"]), Some(&dir)));
    }

    #[test]
    fn a_portal_entry_is_hidden_and_only_written_when_missing() {
        let dir = tmp("portal");
        assert!(ensure_portal_entry("telescope-dev", &argv(&["/x/start.sh"]), Some(&dir)));
        let path = dir.join("applications/telescope-dev.desktop");
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("NoDisplay=true\n"));
        std::fs::write(&path, "mine").unwrap();
        assert!(ensure_portal_entry("telescope-dev", &argv(&["/y/start.sh"]), Some(&dir)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");
    }

    #[derive(Default)]
    struct FakeKey(RefCell<Option<String>>);

    impl RunKey for FakeKey {
        fn get(&self) -> Result<Option<String>, String> {
            Ok(self.0.borrow().clone())
        }
        fn set(&self, line: &str) -> Result<(), String> {
            *self.0.borrow_mut() = Some(line.into());
            Ok(())
        }
        fn delete(&self) -> Result<(), String> {
            *self.0.borrow_mut() = None;
            Ok(())
        }
    }

    #[test]
    fn windows_run_key_round_trip() {
        let key = FakeKey::default();
        let place = Place::Registry(&key);
        assert!(!is_enabled(&place));
        enable(&argv(&[r"C:\Program Files\Telescope\TelescopeDesktop.exe", "--minimized"]), &place).unwrap();
        assert_eq!(key.0.borrow().as_deref(), Some(r#""C:\Program Files\Telescope\TelescopeDesktop.exe" --minimized"#));
        assert!(is_enabled(&place));
        assert_eq!(disable(&place), Ok(()));
        assert_eq!(disable(&place), Ok(()));
        assert!(!is_enabled(&place));
        assert!(!refresh(&argv(&[r"C:\New\TelescopeDesktop.exe"]), &place));
        enable(&argv(&[r"C:\Old\TelescopeDesktop.exe"]), &place).unwrap();
        assert!(refresh(&argv(&[r"C:\New\TelescopeDesktop.exe"]), &place));
        assert_eq!(key.0.borrow().as_deref(), Some(r"C:\New\TelescopeDesktop.exe"));
    }
}
