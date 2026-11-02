//! Host::pick_file through the desktop's file chooser portal (org.freedesktop.portal.FileChooser), Python's
//! QFileDialog. The dialog runs on a thread; the answer comes back to the plugin as Event::RequestDone.

use serde_json::{json, Value};
use telescope_core::host::FileDialog;
#[cfg(target_os = "linux")]
use telescope_core::host::FileDialogKind;

/// "LUT files (*.cube *.3dl)" as the portal's (name, [(0 = glob, pattern)]).
pub fn portal_filter(filter: &str) -> (String, Vec<(u32, String)>) {
    let (name, globs) = match (filter.rfind('('), filter.ends_with(')')) {
        (Some(i), true) => (filter[..i].trim(), &filter[i + 1..filter.len() - 1]),
        _ => (filter.trim(), "*"),
    };
    (name.to_string(), globs.split_whitespace().map(|g| (0, g.to_string())).collect())
}

/// The answer a plugin gets: ok with {"path", "filter"}, or not ok with an empty message for a cancel (a reason
/// when there's no dialog to show).
pub type Answer = (bool, String, Value);

/// The path of a file:// URI, with %XX escapes undone.
pub fn uri_path(uri: &str) -> String {
    let raw = uri.strip_prefix("file://").unwrap_or(uri).as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (raw[i], raw.get(i + 1).and_then(|&b| hex(b)), raw.get(i + 2).and_then(|&b| hex(b))) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Shows the dialog and waits for the person. Blocking.
#[cfg(target_os = "linux")]
pub fn ask(dialog: &FileDialog) -> Answer {
    match zbus::blocking::Connection::session().map_err(|e| e.to_string()).and_then(|conn| portal::ask(&conn, dialog)) {
        Ok(answer) => answer,
        Err(e) => {
            log::info!("No file dialog: {e}");
            (false, e, json!({}))
        }
    }
}

#[cfg(windows)]
pub fn ask(dialog: &FileDialog) -> Answer {
    match windows_dialog::ask(dialog) {
        Ok(answer) => answer,
        Err(e) => {
            log::info!("No file dialog: {e}");
            (false, e.to_string(), json!({}))
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn ask(_dialog: &FileDialog) -> Answer {
    (false, "no file dialog in this build".into(), json!({}))
}

/// The common item dialog (IFileOpenDialog, IFileSaveDialog), what QFileDialog shows on Windows.
#[cfg(windows)]
mod windows_dialog {
    use super::{portal_filter, Answer};
    use serde_json::json;
    use std::path::Path;
    use telescope_core::host::{FileDialog, FileDialogKind};
    use windows::core::{Interface, HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem,
                                    SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH};

    fn folder_item(folder: &Path) -> Option<IShellItem> {
        // SAFETY: the path is a live HSTRING for the call
        folder.is_dir().then(|| unsafe { SHCreateItemFromParsingName(&HSTRING::from(folder.as_os_str()), None) }.ok()).flatten()
    }

    pub fn ask(dialog: &FileDialog) -> windows::core::Result<Answer> {
        // SAFETY: COM is set up for this thread (the dialog's own) and taken down once it's done
        unsafe {
            let inited = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let answer = show(dialog);
            if inited {
                CoUninitialize();
            }
            answer
        }
    }

    unsafe fn show(dialog: &FileDialog) -> windows::core::Result<Answer> {
        let d: IFileDialog = match dialog.kind {
            FileDialogKind::Save => CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?.cast()?,
            _ => CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?.cast()?,
        };
        d.SetTitle(&HSTRING::from(dialog.title.as_str()))?;
        // The texts stay alive until the dialog is done with them
        let specs: Vec<(HSTRING, HSTRING)> = dialog.filters.iter().map(|f| {
            let (name, globs) = portal_filter(f);
            (HSTRING::from(name), HSTRING::from(globs.into_iter().map(|(_, g)| g).collect::<Vec<_>>().join(";")))
        }).collect();
        if !specs.is_empty() && dialog.kind != FileDialogKind::Folder {
            let list: Vec<COMDLG_FILTERSPEC> = specs.iter().map(|(n, s)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(s.as_ptr()) }).collect();
            d.SetFileTypes(&list)?;
            d.SetFileTypeIndex(1)?;
        }
        let start = Path::new(&dialog.start);
        match dialog.kind {
            FileDialogKind::Save => {
                let (folder, name) = if start.is_dir() || dialog.start.ends_with(['/', '\\']) {
                    (start.to_path_buf(), None)
                } else {
                    (start.parent().map(Path::to_path_buf).unwrap_or_default(), start.file_name().map(|n| n.to_string_lossy().into_owned()))
                };
                if let Some(name) = name {
                    d.SetFileName(&HSTRING::from(name))?;
                }
                if let Some(item) = folder_item(&folder) {
                    d.SetFolder(&item)?;
                }
            }
            FileDialogKind::Open | FileDialogKind::Folder => {
                if let Some(item) = folder_item(start) {
                    d.SetFolder(&item)?;
                }
                if dialog.kind == FileDialogKind::Folder {
                    d.SetOptions(d.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)?;
                }
            }
        }
        if let Err(e) = d.Show(None) {
            // Cancel isn't a problem to tell anyone about
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Ok((false, String::new(), json!({})));
            }
            return Err(e);
        }
        let raw = d.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = raw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(raw.0 as *const _));
        let picked = d.GetFileTypeIndex().ok().and_then(|i| dialog.filters.get((i as usize).checked_sub(1)?).cloned());
        let filter = picked.or_else(|| dialog.filters.first().cloned());
        Ok((true, String::new(), json!({"path": path, "filter": filter})))
    }
}

#[cfg(target_os = "linux")]
mod portal {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;
    use zbus::blocking::{Connection, MessageIterator};
    use zbus::zvariant::{OwnedValue, Value as ZValue};
    use zbus::MatchRule;

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const REQUEST: &str = "org.freedesktop.portal.Request";

    /// A NUL-ended byte string, as the portal takes folders and file names.
    fn bytes(s: &str) -> Vec<u8> {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        b
    }

    pub fn ask(conn: &Connection, dialog: &FileDialog) -> Result<Answer, String> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let token = format!("telescope_file_{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let sender = conn.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
        let handle = format!("{PATH}/request/{sender}/{token}");
        let rule = MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(REQUEST)
            .and_then(|r| r.member("Response")).and_then(|r| r.path(handle.as_str())).map(|r| r.build())
            .map_err(|e| e.to_string())?;
        let responses = MessageIterator::for_match_rule(rule, conn, Some(4)).map_err(|e| e.to_string())?;

        let filters: Vec<(String, Vec<(u32, String)>)> = dialog.filters.iter().map(|f| portal_filter(f)).collect();
        let mut options: HashMap<&str, ZValue> = HashMap::from([("handle_token", ZValue::from(token.as_str())), ("modal", ZValue::from(true))]);
        if !filters.is_empty() {
            options.insert("filters", ZValue::from(filters.clone()));
            options.insert("current_filter", ZValue::from(filters[0].clone()));
        }
        let start = Path::new(&dialog.start);
        let method = match dialog.kind {
            FileDialogKind::Save => {
                let (folder, name) = if start.is_dir() || dialog.start.ends_with('/') {
                    (start.to_path_buf(), None)
                } else {
                    (start.parent().map(Path::to_path_buf).unwrap_or_default(), start.file_name().map(|n| n.to_string_lossy().into_owned()))
                };
                if let Some(name) = name {
                    options.insert("current_name", ZValue::from(name));
                }
                if folder.is_dir() {
                    options.insert("current_folder", ZValue::from(bytes(&folder.to_string_lossy())));
                }
                "SaveFile"
            }
            FileDialogKind::Open | FileDialogKind::Folder => {
                if start.is_dir() {
                    options.insert("current_folder", ZValue::from(bytes(&dialog.start)));
                }
                if dialog.kind == FileDialogKind::Folder {
                    options.insert("directory", ZValue::from(true));
                }
                "OpenFile"
            }
        };
        conn.call_method(Some(PORTAL), PATH, Some("org.freedesktop.portal.FileChooser"), method, &("", dialog.title.as_str(), options))
            .map_err(|e| e.to_string())?;
        // The person takes as long as they take
        for msg in responses.flatten() {
            let Ok((code, results)) = msg.body().deserialize::<(u32, HashMap<String, OwnedValue>)>() else { continue };
            if code != 0 {
                return Ok((false, String::new(), json!({})));
            }
            let uris: Vec<String> = results.get("uris").and_then(|v| Vec::<String>::try_from(v.clone()).ok()).unwrap_or_default();
            let Some(uri) = uris.first() else { return Ok((false, String::new(), json!({}))) };
            let filter = results.get("current_filter")
                .and_then(|v| <(String, Vec<(u32, String)>)>::try_from(v.clone()).ok())
                .and_then(|picked| dialog.filters.iter().zip(&filters).find(|(_, f)| f.0 == picked.0).map(|(q, _)| q.clone()))
                .or_else(|| dialog.filters.first().cloned())
                .unwrap_or_default();
            return Ok((true, String::new(), json!({"path": uri_path(uri), "filter": filter})));
        }
        Err("the portal went away".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qt_filters_become_the_portals() {
        assert_eq!(portal_filter("LUT files (*.cube *.3dl)"), ("LUT files".into(), vec![(0, "*.cube".into()), (0, "*.3dl".into())]));
        assert_eq!(portal_filter("All files"), ("All files".into(), vec![(0, "*".into())]));
    }

    /// What the fake portal was asked.
    #[cfg(target_os = "linux")]
    type Calls = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A private session bus with a FileChooser that answers every dialog with answer (code, uri), recording the calls.
    #[cfg(target_os = "linux")]
    fn fake_portal(answer: (u32, &'static str)) -> Option<(std::process::Child, String, Calls)> {
        use std::io::BufRead;
        use zbus::blocking::{Connection, MessageIterator};
        use zbus::zvariant::{OwnedValue, Value as Z};
        let mut daemon = std::process::Command::new("dbus-daemon").args(["--session", "--nofork", "--print-address"])
            .stdout(std::process::Stdio::piped()).spawn().ok()?;
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take()?).read_line(&mut address).ok()?;
        let address = address.trim().to_string();
        let conn = zbus::blocking::connection::Builder::address(address.as_str()).ok()?.build().ok()?;
        conn.request_name("org.freedesktop.portal.Desktop").ok()?;
        let calls: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let seen = calls.clone();
        std::thread::spawn(move || {
            let c: Connection = conn;
            for msg in MessageIterator::from(&c).flatten() {
                if msg.message_type() != zbus::message::Type::MethodCall {
                    continue;
                }
                let header = msg.header();
                let member = header.member().map(|m| m.to_string()).unwrap_or_default();
                let sender = header.sender().map(|s| s.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
                let Ok((_, title, opts)) = msg.body().deserialize::<(String, String, std::collections::HashMap<String, OwnedValue>)>() else { continue };
                let token = String::try_from(opts["handle_token"].clone()).unwrap();
                let name = opts.get("current_name").and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
                seen.lock().unwrap().push(format!("{member} {title} {name} dir={}", opts.contains_key("directory")));
                let handle = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
                let path = zbus::zvariant::ObjectPath::try_from(handle.as_str()).unwrap();
                let _ = c.send(&zbus::Message::method_return(&header).unwrap().build(&(path,)).unwrap());
                let results = std::collections::HashMap::from([("uris", Z::from(vec![answer.1]))]);
                let _ = c.emit_signal(None::<zbus::names::BusName>, handle.as_str(), "org.freedesktop.portal.Request", "Response", &(answer.0, results));
            }
        });
        Some((daemon, address, calls))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_portal_asks_for_a_file_and_says_which_was_picked() {
        let Some((mut daemon, address, calls)) = fake_portal((0, "file:///tmp/My%20LUTs/film.cube")) else {
            eprintln!("needs dbus-daemon");
            return;
        };
        let conn = zbus::blocking::connection::Builder::address(address.as_str()).unwrap().build().unwrap();
        let open = FileDialog { kind: FileDialogKind::Open, title: "Load a LUT".into(), start: "/tmp".into(), filters: vec!["LUT files (*.cube)".into()] };
        let (ok, message, data) = portal::ask(&conn, &open).unwrap();
        assert!(ok && message.is_empty());
        assert_eq!(data, json!({"path": "/tmp/My LUTs/film.cube", "filter": "LUT files (*.cube)"}));
        let save = FileDialog { kind: FileDialogKind::Save, title: "Export the grade".into(), start: "/tmp/grade.cube".into(), filters: vec![] };
        assert!(portal::ask(&conn, &save).unwrap().0);
        assert_eq!(*calls.lock().unwrap(), ["OpenFile Load a LUT  dir=false", "SaveFile Export the grade grade.cube dir=false"]);
        let _ = daemon.kill();
        let _ = daemon.wait();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_cancelled_dialog_is_not_ok_with_no_reason() {
        let Some((mut daemon, address, _)) = fake_portal((1, "")) else { return };
        let conn = zbus::blocking::connection::Builder::address(address.as_str()).unwrap().build().unwrap();
        let folder = FileDialog { kind: FileDialogKind::Folder, title: "Pick".into(), start: String::new(), filters: vec![] };
        assert_eq!(portal::ask(&conn, &folder).unwrap(), (false, String::new(), json!({})));
        let _ = daemon.kill();
        let _ = daemon.wait();
    }

    #[test]
    fn a_picked_uri_is_a_path() {
        assert_eq!(uri_path("file:///home/me/My%20LUTs/film.cube"), "/home/me/My LUTs/film.cube");
    }
}
