//! Installing Telescope's camera on Windows (platform/windows.py install_camera): the Windows 11 camera where it can be,
//! else UnityCapture, downloaded first when it isn't here yet. Either way the DLLs go into Program Files\Telescope as
//! admin, are checked there against their hashes and registered from there, so nothing can swap one between the check
//! and a later load. The admin side is a PowerShell script run through Windows' elevation prompt; the scripts are
//! Python's, word for word.
//!
//! The scripts and the steps are plain code behind InstallSys, so tests run them anywhere.

use crate::wincam::{MF_DLL, UC_NAME};
use std::path::{Path, PathBuf};
use telescope_core::i18n::tr;

/// UnityCapture at the commit whose DLLs the hashes below are of.
pub const UC_URL_BASE: &str = "https://raw.githubusercontent.com/schellingb/UnityCapture/3ed54c325e0ad71afcf4f246c07e5e17b3d7f2d2/Install";
pub const UC_SHA256: [(&str, &str); 2] = [
    ("UnityCaptureFilter32.dll", "aa3ebdf03dea7f3aab3dd7b724751f49ed71672256b57c6a19aa6809cabf30ba"),
    ("UnityCaptureFilter64.dll", "72812f5363d8ecb45632253f8c8c888844b1b62e27616f3c8cc21064ccde25e5"),
];
pub const EXIT_CHECKSUM: i32 = 2;
pub const EXIT_REGSVR32: i32 = 3;
pub const EXIT_UNSAFE_FOLDER: i32 = 4;
pub const EXIT_ERROR: i32 = 5;

/// What installing needs from Windows, the network and the disk.
pub trait InstallSys {
    /// The Windows 11 camera can be set up here: Windows 11, not asked for the older one, and its DLL beside the app.
    fn mf_installable(&self) -> bool;
    /// Slot 0's Windows 11 camera is registered.
    fn mf_registered(&self) -> bool;
    /// Windows' camera access switch is off for the whole computer.
    fn camera_access_off(&self) -> bool;
    /// Where this copy keeps the Windows 11 camera's DLL and UnityCapture's.
    fn mf_dir(&self) -> PathBuf;
    fn uc_dir(&self) -> PathBuf;
    /// The folder an older Telescope registered UnityCapture from, when it isn't Program Files.
    fn uc_user_folder(&self) -> Option<PathBuf>;
    /// A file's SHA-256 as lower-case hex, or None when it can't be read.
    fn sha256(&self, path: &Path) -> Option<String>;
    fn remove(&self, path: &Path);
    /// Downloads url to dest; Err with why.
    fn download(&self, url: &str, dest: &Path) -> Result<(), String>;
    fn make_dir(&self, path: &Path) -> Result<(), String>;
    fn rename(&self, from: &Path, to: &Path) -> Result<(), String>;
    /// Runs script as admin after Windows' prompt: its exit code, or Err when it couldn't be started or timed out.
    fn run_as_admin(&self, script: &str) -> Result<i32, String>;
}

/// PowerShell's single-quoted string.
pub fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The script that runs as admin: find Program Files itself, refuse a Telescope folder there that isn't admin-only all
/// the way down, copy the DLLs into folder in it, check their hashes there, then run register (PowerShell) on those
/// copies (_admin_install_script).
pub fn admin_install_script(src: &Path, folder: &str, digests: &[(&str, &str)], register: &str) -> String {
    let files = digests.iter().map(|(n, d)| format!("{} = {}", ps_quote(n), ps_quote(d))).collect::<Vec<_>>().join("; ");
    let src = ps_quote(&src.to_string_lossy());
    let folder = ps_quote(folder);
    format!(r#"$ErrorActionPreference = 'Stop'
# Anything unexpected gets its own exit code, so it isn't mistaken for a cancelled admin prompt
trap {{ exit {EXIT_ERROR} }}
# Started from PowerShell 7, this inherits its module path and can't load its own modules: use the system's
$env:PSModulePath = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
# .NET directly rather than Get-FileHash and Get-Acl, so nothing here depends on loading a module
function Get-Sha256($path) {{
    $stream = [IO.File]::OpenRead($path)
    try {{ return -join ([Security.Cryptography.SHA256]::Create().ComputeHash($stream) | ForEach-Object {{ $_.ToString('X2') }}) }}
    finally {{ $stream.Dispose() }}
}}
$src = {src}
# Resolved here, as admin, from the system's settings: nothing the unelevated app passes decides where the DLLs go
$telescope = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Telescope'
$dst = Join-Path $telescope {folder}
$files = [ordered]@{{ {files} }}
$icacls = Join-Path ([Environment]::SystemDirectory) 'icacls.exe'
$sid = [Security.Principal.SecurityIdentifier]
# Administrators, SYSTEM, TrustedInstaller
$trusted = @('S-1-5-32-544', 'S-1-5-18', 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
# Write data or attributes, delete, change permissions or owner, and generic all/write
$writes = 0x2 -bor 0x4 -bor 0x10 -bor 0x40 -bor 0x100 -bor 0x10000 -bor 0x40000 -bor 0x80000 -bor 0x10000000 -bor 0x40000000

# Admin-owned, no link anywhere, and nobody else can change it: checked, never repaired, so a link is never followed
function Test-AdminOnly($path) {{
    $item = Get-Item -LiteralPath $path -Force
    if (([int]$item.Attributes -band 0x400) -ne 0) {{ return $false }}  # FILE_ATTRIBUTE_REPARSE_POINT
    $acl = $item.GetAccessControl()
    if ($trusted -notcontains $acl.GetOwner($sid).Value) {{ return $false }}
    foreach ($rule in $acl.GetAccessRules($true, $true, $sid)) {{
        if ($rule.AccessControlType -ne 'Allow' -or ([int]$rule.PropagationFlags -band 2) -ne 0) {{ continue }}  # 2: InheritOnly
        if (([int]$rule.FileSystemRights -band $writes) -ne 0 -and $trusted -notcontains $rule.IdentityReference.Value) {{ return $false }}
    }}
    if ($item.PSIsContainer) {{
        foreach ($child in [IO.Directory]::EnumerateFileSystemEntries($path)) {{
            if (-not (Test-AdminOnly $child)) {{ return $false }}
        }}
    }}
    return $true
}}

# Only for what this script just made: Windows can be set to make the creating account the owner instead
function Set-AdminOwner($path) {{
    & $icacls $path /setowner '*S-1-5-32-544' /Q | Out-Null
    if ($LASTEXITCODE -ne 0) {{ exit {EXIT_UNSAFE_FOLDER} }}
}}

if ((Test-Path -LiteralPath $telescope) -and -not (Test-AdminOnly $telescope)) {{ exit {EXIT_UNSAFE_FOLDER} }}
foreach ($dir in @($telescope, $dst)) {{
    if (-not (Test-Path -LiteralPath $dir)) {{
        New-Item -ItemType Directory -Path $dir | Out-Null
        Set-AdminOwner $dir
    }}
}}
# Old copies set aside by an earlier update, once nothing has them loaded any more
Get-ChildItem -LiteralPath $dst -Filter '*.old' -Force | Remove-Item -Force -ErrorAction SilentlyContinue
foreach ($name in $files.Keys) {{
    $target = Join-Path $dst $name
    # A copy already there and right is kept: a camera app may have it loaded, so it can't be overwritten
    if (-not (Test-Path -LiteralPath $target) -or (Get-Sha256 $target) -ne $files[$name]) {{
        # A loaded one can still be renamed out of the way
        if (Test-Path -LiteralPath $target) {{ Move-Item -LiteralPath $target -Destination ($target + '.' + [guid]::NewGuid() + '.old') -Force }}
        Copy-Item -LiteralPath (Join-Path $src $name) -Destination $target -Force
        Set-AdminOwner $target
    }}
}}
# Again over the finished tree, so nothing that changed while it was being made gets registered
if (-not (Test-AdminOnly $telescope)) {{ exit {EXIT_UNSAFE_FOLDER} }}
foreach ($name in $files.Keys) {{
    $target = Join-Path $dst $name
    if ((Get-Sha256 $target) -ne $files[$name]) {{
        Remove-Item -LiteralPath $target -Force
        exit {EXIT_CHECKSUM}
    }}
}}
$regsvr32 = Join-Path ([Environment]::SystemDirectory) 'regsvr32.exe'
{register}
exit 0
"#)
}

/// UnityCapture's DLLs into Program Files\Telescope\UnityCapture, registered from there for devices cameras; reset
/// unregisters them all first, since registering never takes cameras away (_elevated_install_script).
pub fn uc_install_script(src: &Path, devices: u32, reset: bool) -> String {
    // UnityCapture reads both from regsvr32's command line; the extra cameras get "Telescope #2" and on
    let count = if devices > 1 { format!("'/i:UnityCaptureDevices={devices}', ") } else { String::new() };
    let unregister = if reset { "$p = Start-Process $regsvr32 -ArgumentList '/s', '/u', $dll -Wait -PassThru\n    " } else { "" };
    let register = format!(r#"foreach ($name in $files.Keys) {{
    $dll = '"' + (Join-Path $dst $name) + '"'
    {unregister}$p = Start-Process $regsvr32 -ArgumentList '/s', {count}'"/i:UnityCaptureName={UC_NAME}"', $dll -Wait -PassThru
    if ($p.ExitCode -ne 0) {{ exit {EXIT_REGSVR32} }}
}}"#);
    admin_install_script(src, "UnityCapture", &UC_SHA256, &register)
}

/// The Windows 11 camera's DLL into Program Files\Telescope\VirtualCamera, set up from there for devices cameras, then
/// UnityCapture unregistered, as apps would list Telescope twice. uc_dir: the folder an older Telescope registered
/// UnityCapture from, if not Program Files, unregistered from a checked copy, never run from there (_mf_install_script).
pub fn mf_install_script(src: &Path, digest: &str, devices: u32, uc_dir: Option<&Path>) -> String {
    let uc = UC_SHA256.iter().map(|(n, h)| format!("{} = {}", ps_quote(n), ps_quote(h))).collect::<Vec<_>>().join("; ");
    let from_user_folder = match uc_dir {
        Some(dir) => format!(r#"$from = Join-Path {} $name
            if (-not (Test-Path -LiteralPath $from)) {{ continue }}
            $old = $copy = Join-Path $dst ($name + '.' + [guid]::NewGuid() + '.old')
            Copy-Item -LiteralPath $from -Destination $copy -Force
            if ((Get-Sha256 $copy) -ne $uc[$name]) {{ Remove-Item -LiteralPath $copy -Force; continue }}"#, ps_quote(&dir.to_string_lossy())),
        None => "continue".into(),
    };
    let register = format!(r#"$p = Start-Process $regsvr32 -ArgumentList '/s', '/n', '/i:{devices}', ('"' + (Join-Path $dst {dll}) + '"') -Wait -PassThru
if ($p.ExitCode -ne 0) {{ exit {EXIT_REGSVR32} }}
$uc = @{{ {uc} }}
foreach ($name in $uc.Keys) {{
    try {{
        $old = Join-Path (Join-Path $telescope 'UnityCapture') $name
        $copy = $null
        if (-not (Test-Path -LiteralPath $old)) {{
            {from_user_folder}
        }}
        Start-Process $regsvr32 -ArgumentList '/s', '/u', ('"' + $old + '"') -Wait
        if ($copy) {{ Remove-Item -LiteralPath $copy -Force -ErrorAction SilentlyContinue }}
    }} catch {{ }}  # the new camera is in either way
}}"#, dll = ps_quote(MF_DLL));
    admin_install_script(src, "VirtualCamera", &[(MF_DLL, digest)], &register)
}

/// The unelevated PowerShell command that starts script as admin and passes its exit code back (_run_as_admin).
pub fn elevate_command(script: &str) -> String {
    let units: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64(&units);
    format!("$ErrorActionPreference = 'Stop'; \
             $ps = Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell\\v1.0\\powershell.exe'; \
             $p = Start-Process $ps -ArgumentList '-NoProfile', '-NonInteractive', '-EncodedCommand', '{encoded}' \
             -Verb RunAs -Wait -PassThru -WindowStyle Hidden; exit $p.ExitCode")
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() { TABLE[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

/// What an admin run's exit code means: (ok, what to say).
pub fn admin_result(code: i32) -> (bool, String) {
    match code {
        0 => (true, tr("Installed")),
        EXIT_CHECKSUM => (false, tr("The driver copy in Program Files failed checksum verification - not registering")),
        EXIT_ERROR => (false, tr("The driver install hit an error while running as admin - not registering")),
        EXIT_UNSAFE_FOLDER => (false, tr("Program Files\\Telescope has links in it or other users can change it - delete that folder, then install again")),
        EXIT_REGSVR32 => (false, tr("Windows refused to register the driver (regsvr32 failed)")),
        _ => (false, tr("Registration failed (cancelled or denied?)")),
    }
}

fn run_admin(sys: &dyn InstallSys, script: &str) -> (bool, String, Option<i32>) {
    match sys.run_as_admin(script) {
        Ok(code) => {
            let (ok, msg) = admin_result(code);
            (ok, msg, Some(code))
        }
        Err(why) => (false, why, None),
    }
}

/// Downloads UnityCapture's DLLs beside the app, each checked against its hash (download_unitycapture).
pub fn download_unitycapture(sys: &dyn InstallSys, progress: &dyn Fn(&str)) -> (bool, String) {
    let dir = sys.uc_dir();
    if let Err(e) = sys.make_dir(&dir) {
        return (false, tr("Couldn't create {path}: {error}").replace("{path}", &dir.to_string_lossy()).replace("{error}", &e));
    }
    for (name, want) in UC_SHA256 {
        let (dest, part) = (dir.join(name), dir.join(format!("{name}.part")));
        progress(&tr("Downloading {file}...").replace("{file}", name));
        let failed = |e: String| (false, tr("Download failed: {error}").replace("{error}", &e));
        // A cut-off download never sits where the callers look for a finished one
        if let Err(e) = sys.download(&format!("{UC_URL_BASE}/{name}"), &part) {
            sys.remove(&part);
            return failed(e);
        }
        let digest = sys.sha256(&part).unwrap_or_default();
        if digest != want {
            sys.remove(&part);
            return (false, tr("{file} failed checksum verification (got {digest}...) - not registering")
                .replace("{file}", name).replace("{digest}", digest.get(..12).unwrap_or(&digest)));
        }
        if let Err(e) = sys.rename(&part, &dest) {
            sys.remove(&part);
            return failed(e);
        }
    }
    (true, tr("Downloaded"))
}

fn register_unitycapture(sys: &dyn InstallSys, devices: u32, reset: bool) -> (bool, String) {
    let dir = sys.uc_dir();
    for (name, want) in UC_SHA256 {
        let path = dir.join(name);
        if sys.sha256(&path).as_deref() != Some(want) {
            sys.remove(&path); // so "Try again" downloads it afresh instead of failing the same way
            return (false, tr("{file} failed checksum verification - not registering").replace("{file}", name));
        }
    }
    // The check above is only for a quick, clear failure; the one that counts runs as admin on the protected copy
    let (ok, msg, _) = run_admin(sys, &uc_install_script(&dir, devices, reset));
    (ok, msg)
}

/// Sets up Telescope's camera as admin with devices cameras to stream to: the Windows 11 one where it can be, else
/// UnityCapture (downloaded first if needed). reset: UnityCapture goes back to fewer cameras.
pub fn install_camera(sys: &dyn InstallSys, devices: u32, reset: bool, progress: &dyn Fn(&str)) -> (bool, String) {
    let registering = tr("Registering (admin access required)...");
    if sys.mf_installable() {
        progress(&registering);
        let src = sys.mf_dir();
        // Hashed here and checked again as admin on the copy in Program Files, the one that gets registered
        let digest = sys.sha256(&src.join(MF_DLL)).unwrap_or_default().to_uppercase();
        let user_folder = sys.uc_user_folder();
        let (ok, msg, code) = run_admin(sys, &mf_install_script(&src, &digest, devices, user_folder.as_deref()));
        if ok || code != Some(EXIT_REGSVR32) {
            return (ok, msg);
        }
        if sys.mf_registered() {
            // The cameras there stay as they were, so UnityCapture would list Telescope twice
            return (false, if sys.camera_access_off() {
                tr("Camera access is off in Windows (Settings > Privacy & security > Camera). Turn it on, then try again")
            } else { msg });
        }
        log::warn!("Windows didn't take the Windows 11 camera ({msg}); installing UnityCapture instead");
    }
    let dir = sys.uc_dir();
    // Both, since a bad one is deleted on its own: checking one would skip a download the other still needs
    if !UC_SHA256.iter().all(|(name, _)| sys.sha256(&dir.join(name)).is_some()) {
        progress(&tr("Downloading driver files..."));
        let (ok, msg) = download_unitycapture(sys, progress);
        if !ok {
            return (false, msg);
        }
    }
    progress(&registering);
    register_unitycapture(sys, devices, reset)
}

/// Where this copy of Telescope keeps the DLLs it would install: beside the executable, as the frozen Python app did.
pub fn app_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
}

#[cfg(windows)]
pub use real::Windows;

#[cfg(windows)]
mod real {
    use super::*;
    use crate::wincam::{self, Registry};
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::time::{Duration, Instant};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const ADMIN_TIMEOUT: Duration = Duration::from_secs(60);
    const CONSENT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\webcam";

    /// The real thing: this computer's registry, disk and network.
    pub struct Windows;

    impl InstallSys for Windows {
        fn mf_installable(&self) -> bool {
            wincam::mf_allowed(&wincam::System) && self.mf_dir().join(MF_DLL).is_file()
        }
        fn mf_registered(&self) -> bool {
            wincam::mf_registered(&wincam::System, 0)
        }
        fn camera_access_off(&self) -> bool {
            wincam::System.machine_string(CONSENT, "Value").as_deref() == Some("Deny")
        }
        fn mf_dir(&self) -> PathBuf {
            app_dir().join("virtualcamera")
        }
        fn uc_dir(&self) -> PathBuf {
            app_dir().join("unitycapture")
        }
        fn uc_user_folder(&self) -> Option<PathBuf> {
            // Telescope before 3.1.2 registered it from beside the app; the script only touches a copy that checks out
            let dir = self.uc_dir();
            dir.join(UC_SHA256[1].0).is_file().then_some(dir)
        }
        fn sha256(&self, path: &Path) -> Option<String> {
            let mut file = std::fs::File::open(path).ok()?;
            let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = file.read(&mut buf).ok()?;
                if n == 0 {
                    break;
                }
                ctx.update(&buf[..n]);
            }
            Some(ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
        }
        fn remove(&self, path: &Path) {
            let _ = std::fs::remove_file(path);
        }
        fn download(&self, url: &str, dest: &Path) -> Result<(), String> {
            let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
            let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
            let mut reader = response.body_mut().as_reader();
            let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
            std::io::copy(&mut reader, &mut file).map_err(|e| e.to_string())?;
            Ok(())
        }
        fn make_dir(&self, path: &Path) -> Result<(), String> {
            std::fs::create_dir_all(path).map_err(|e| e.to_string())
        }
        fn rename(&self, from: &Path, to: &Path) -> Result<(), String> {
            std::fs::rename(from, to).map_err(|e| e.to_string())
        }
        fn run_as_admin(&self, script: &str) -> Result<i32, String> {
            // Windows' own PowerShell, from its system folder rather than PATH
            let powershell = crate::win::system_dir().map(|d| d.join(r"WindowsPowerShell\v1.0\powershell.exe"))
                .unwrap_or_else(|| PathBuf::from("powershell"));
            let mut child = std::process::Command::new(powershell).args(["-NoProfile", "-Command", &elevate_command(script)])
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                .creation_flags(CREATE_NO_WINDOW).spawn().map_err(|e| e.to_string())?;
            let started = Instant::now();
            loop {
                if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                    return Ok(status.code().unwrap_or(-1));
                }
                if started.elapsed() > ADMIN_TIMEOUT {
                    let _ = child.kill();
                    return Err(tr("Timed out"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake {
        mf_installable: bool,
        mf_registered: bool,
        access_off: bool,
        files: RefCell<HashMap<PathBuf, String>>,
        served: HashMap<String, String>,
        exit_codes: RefCell<Vec<i32>>,
        scripts: RefCell<Vec<String>>,
        steps: RefCell<Vec<String>>,
    }

    impl InstallSys for Fake {
        fn mf_installable(&self) -> bool { self.mf_installable }
        fn mf_registered(&self) -> bool { self.mf_registered }
        fn camera_access_off(&self) -> bool { self.access_off }
        fn mf_dir(&self) -> PathBuf { PathBuf::from("C:/app/virtualcamera") }
        fn uc_dir(&self) -> PathBuf { PathBuf::from("C:/app/unitycapture") }
        fn uc_user_folder(&self) -> Option<PathBuf> { None }
        fn sha256(&self, path: &Path) -> Option<String> { self.files.borrow().get(path).cloned() }
        fn remove(&self, path: &Path) { self.files.borrow_mut().remove(path); }
        fn download(&self, url: &str, dest: &Path) -> Result<(), String> {
            self.steps.borrow_mut().push(format!("download {}", url.rsplit('/').next().unwrap()));
            let digest = self.served.get(url.rsplit('/').next().unwrap()).cloned().ok_or("404")?;
            self.files.borrow_mut().insert(dest.to_path_buf(), digest);
            Ok(())
        }
        fn make_dir(&self, _path: &Path) -> Result<(), String> { Ok(()) }
        fn rename(&self, from: &Path, to: &Path) -> Result<(), String> {
            let d = self.files.borrow_mut().remove(from).ok_or("missing")?;
            self.files.borrow_mut().insert(to.to_path_buf(), d);
            Ok(())
        }
        fn run_as_admin(&self, script: &str) -> Result<i32, String> {
            self.scripts.borrow_mut().push(script.to_string());
            Ok(self.exit_codes.borrow_mut().remove(0))
        }
    }

    fn good_downloads() -> HashMap<String, String> {
        UC_SHA256.iter().map(|(n, d)| (n.to_string(), d.to_string())).collect()
    }

    #[test]
    fn windows_11_installs_its_own_camera_from_program_files() {
        let fake = Fake { mf_installable: true, exit_codes: RefCell::new(vec![0]), ..Default::default() };
        fake.files.borrow_mut().insert(PathBuf::from("C:/app/virtualcamera").join(MF_DLL), "abc123".into());
        assert_eq!(install_camera(&fake, 2, false, &|_| {}), (true, "Installed".into()));
        let script = &fake.scripts.borrow()[0];
        assert!(script.contains("$dst = Join-Path $telescope 'VirtualCamera'"));
        assert!(script.contains("$files = [ordered]@{ 'TelescopeCamera.dll' = 'ABC123' }"), "hashed here, upper case, checked as admin");
        assert!(script.contains("'/s', '/n', '/i:2'"), "two cameras");
        assert!(fake.steps.borrow().is_empty(), "nothing downloaded");
    }

    #[test]
    fn a_refused_windows_11_camera_falls_back_to_unitycapture_unless_its_cameras_are_still_there() {
        let fake = Fake { mf_installable: true, served: good_downloads(), exit_codes: RefCell::new(vec![EXIT_REGSVR32, 0]), ..Default::default() };
        assert_eq!(install_camera(&fake, 1, false, &|_| {}), (true, "Installed".into()));
        assert_eq!(*fake.steps.borrow(), ["download UnityCaptureFilter32.dll", "download UnityCaptureFilter64.dll"]);
        assert!(fake.scripts.borrow()[1].contains("\"/i:UnityCaptureName=Telescope\""));

        let kept = Fake { mf_installable: true, mf_registered: true, access_off: true, exit_codes: RefCell::new(vec![EXIT_REGSVR32]), ..Default::default() };
        let (ok, msg) = install_camera(&kept, 1, false, &|_| {});
        assert!(!ok && msg.starts_with("Camera access is off in Windows"), "{msg}");
        assert_eq!(kept.scripts.borrow().len(), 1, "UnityCapture would list Telescope twice");

        let cancelled = Fake { mf_installable: true, exit_codes: RefCell::new(vec![1223]), ..Default::default() };
        assert_eq!(install_camera(&cancelled, 1, false, &|_| {}), (false, "Registration failed (cancelled or denied?)".into()));
    }

    #[test]
    fn a_download_that_doesnt_check_out_is_never_registered() {
        let mut served = good_downloads();
        served.insert("UnityCaptureFilter64.dll".into(), "0123456789abcdef".into());
        let fake = Fake { served, ..Default::default() };
        let (ok, msg) = install_camera(&fake, 1, false, &|_| {});
        assert!(!ok);
        assert_eq!(msg, "UnityCaptureFilter64.dll failed checksum verification (got 0123456789ab...) - not registering");
        assert!(fake.scripts.borrow().is_empty());
        assert!(!fake.files.borrow().keys().any(|p| p.to_string_lossy().ends_with(".part")), "no part file left");
        // Going back to fewer cameras unregisters first
        let reset = uc_install_script(Path::new("C:/app/unitycapture"), 1, true);
        assert!(reset.contains("'/s', '/u', $dll"));
        assert!(!reset.contains("UnityCaptureDevices"));
        assert!(uc_install_script(Path::new("C:/x"), 3, false).contains("'/i:UnityCaptureDevices=3', "));
    }

    #[test]
    fn the_admin_script_starts_encoded_and_quotes_paths() {
        assert_eq!(ps_quote("C:\\Bob's\\app"), "'C:\\Bob''s\\app'");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        let cmd = elevate_command("exit 0");
        // "exit 0" as UTF-16LE, base64
        assert!(cmd.contains("'-EncodedCommand', 'ZQB4AGkAdAAgADAA'"), "{cmd}");
        assert!(cmd.contains("-Verb RunAs -Wait -PassThru -WindowStyle Hidden; exit $p.ExitCode"));
        let script = mf_install_script(Path::new("C:\\it's here"), "AB", 1, Some(Path::new("C:\\old")));
        assert!(script.contains("$src = 'C:\\it''s here'"));
        assert!(script.contains("$from = Join-Path 'C:\\old' $name"));
        assert!(script.starts_with("$ErrorActionPreference = 'Stop'\n# Anything unexpected"));
        assert!(script.trim_end().ends_with("exit 0"));
    }
}
