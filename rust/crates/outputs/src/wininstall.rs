//! Installing Telescope's camera on Windows (platform/windows.py install_camera): the Windows 11 camera where it can be,
//! else UnityCapture, downloaded first when it isn't here yet. Either way the DLLs go into Program Files\Telescope as
//! admin, are checked there against their hashes and registered from there, so nothing can swap one between the check
//! and a later load. The admin side is a PowerShell script run through Windows' elevation prompt; the scripts are
//! Python's, word for word.
//!
//! The scripts and the steps are plain code behind InstallSys, which only hands out what Windows and the network have
//! to answer, so tests run the real thing anywhere.

use telescope_core::i18n::Fields;
use crate::wincam::{MF_DLL, UC_NAME};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
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
/// How long a download may take to answer (urlopen's 30) and how long the admin run gets, prompt included.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
pub const ADMIN_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a process gave no exit code.
#[derive(Debug)]
pub enum RunFailure {
    TimedOut,
    Failed(std::io::Error),
}

/// What installing needs from Windows and the network.
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
    /// UnityCapture's files and the hashes they have to have.
    fn uc_files(&self) -> Vec<(String, String)> {
        UC_SHA256.iter().map(|(n, d)| (n.to_string(), d.to_string())).collect()
    }
    /// Windows' own system folder, from Windows rather than PATH or %SystemRoot%.
    fn system_dir(&self) -> Option<PathBuf>;
    /// Starts fetching url, given at most timeout.
    fn open_url(&self, url: &str, timeout: Duration) -> std::io::Result<Box<dyn Read>>;
    /// Runs program to the end, with no window and at most timeout: its exit code.
    fn run(&self, program: &str, args: &[String], timeout: Duration) -> Result<i32, RunFailure>;
}

/// PowerShell's single-quoted string.
pub fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The script that runs as admin: find Program Files itself, refuse a Telescope folder there that isn't admin-only all
/// the way down, copy the DLLs into folder in it, check their hashes there, then run register (PowerShell) on those
/// copies (_admin_install_script).
pub fn admin_install_script(src: &Path, folder: &str, digests: &[(String, String)], register: &str) -> String {
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
pub fn uc_install_script(src: &Path, digests: &[(String, String)], devices: u32, reset: bool) -> String {
    // UnityCapture reads both from regsvr32's command line; the extra cameras get "Telescope #2" and on
    let count = if devices > 1 { format!("'/i:UnityCaptureDevices={devices}', ") } else { String::new() };
    let unregister = if reset { "$p = Start-Process $regsvr32 -ArgumentList '/s', '/u', $dll -Wait -PassThru\n    " } else { "" };
    let register = format!(r#"foreach ($name in $files.Keys) {{
    $dll = '"' + (Join-Path $dst $name) + '"'
    {unregister}$p = Start-Process $regsvr32 -ArgumentList '/s', {count}'"/i:UnityCaptureName={UC_NAME}"', $dll -Wait -PassThru
    if ($p.ExitCode -ne 0) {{ exit {EXIT_REGSVR32} }}
}}"#);
    admin_install_script(src, "UnityCapture", digests, &register)
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
    admin_install_script(src, "VirtualCamera", &[(MF_DLL.to_string(), digest.to_string())], &register)
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

/// Runs script in PowerShell as admin, after Windows' prompt: (ok, what to say, exit code if it got one) (_run_as_admin).
fn run_admin(sys: &dyn InstallSys, script: &str) -> (bool, String, Option<i32>) {
    let args = ["-NoProfile".to_string(), "-Command".to_string(), elevate_command(script)];
    match sys.run(&powershell_path(sys.system_dir().as_deref()), &args, ADMIN_TIMEOUT) {
        Ok(code) => {
            let (ok, msg) = admin_result(code);
            (ok, msg, Some(code))
        }
        Err(RunFailure::TimedOut) => (false, tr("Timed out"), None),
        Err(RunFailure::Failed(e)) => (false, e.to_string(), None),
    }
}

/// One DLL into its .part file, then its final name once it checks out.
fn download_one(sys: &dyn InstallSys, name: &str, want: &str, part: &Path, dest: &Path) -> Result<(), String> {
    let failed = |e: std::io::Error| tr("Download failed: {error}").field("error", &e.to_string());
    let mut reader = sys.open_url(&format!("{UC_URL_BASE}/{name}"), DOWNLOAD_TIMEOUT).map_err(failed)?;
    let mut file = std::fs::File::create(part).map_err(failed)?;
    std::io::copy(&mut reader, &mut file).map_err(failed)?;
    drop(file);
    let digest = sha256_file(part).ok_or_else(|| failed(std::io::Error::other("can't read the download")))?;
    if digest != want {
        return Err(tr("{file} failed checksum verification (got {digest}...) - not registering")
            .field("file", name).field("digest", &digest[..12]));
    }
    std::fs::rename(part, dest).map_err(failed)
}

/// Downloads UnityCapture's DLLs beside the app, each checked against its hash (download_unitycapture).
pub fn download_unitycapture(sys: &dyn InstallSys, progress: &dyn Fn(&str)) -> (bool, String) {
    let dir = sys.uc_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return (false, tr("Couldn't create {path}: {error}").field("path", dir.to_string_lossy()).field("error", &e.to_string()));
    }
    for (name, want) in sys.uc_files() {
        // A cut-off download never sits where the callers look for a finished one
        let (dest, part) = (dir.join(&name), dir.join(format!("{name}.part")));
        progress(&tr("Downloading {file}...").field("file", &name));
        let result = download_one(sys, &name, &want, &part, &dest);
        let _ = std::fs::remove_file(&part);
        if let Err(why) = result {
            return (false, why);
        }
    }
    (true, tr("Downloaded"))
}

fn register_unitycapture(sys: &dyn InstallSys, devices: u32, reset: bool) -> (bool, String) {
    let dir = sys.uc_dir();
    let files = sys.uc_files();
    for (name, want) in &files {
        let path = dir.join(name);
        if sha256_file(&path).as_deref() != Some(want) {
            let _ = std::fs::remove_file(&path); // so "Try again" downloads it afresh instead of failing the same way
            return (false, tr("{file} failed checksum verification - not registering").field("file", name));
        }
    }
    // The check above is only for a quick, clear failure; the one that counts runs as admin on the protected copy
    let (ok, msg, _) = run_admin(sys, &uc_install_script(&dir, &files, devices, reset));
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
        let digest = sha256_file(&src.join(MF_DLL)).unwrap_or_default().to_uppercase();
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
    if !sys.uc_files().iter().all(|(name, _)| dir.join(name).exists()) {
        progress(&tr("Downloading driver files..."));
        let (ok, msg) = download_unitycapture(sys, &|_| {});
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

/// UnityCapture's DLLs for the copy of Telescope that runs from app (unitycapture_dir).
pub fn unitycapture_dir_in(app: &Path) -> PathBuf {
    app.join("unitycapture")
}

/// The Windows 11 camera's DLL for the copy that runs from app (mf_camera_dir).
pub fn mf_camera_dir_in(app: &Path) -> PathBuf {
    app.join("virtualcamera")
}

pub fn unitycapture_dir() -> PathBuf {
    unitycapture_dir_in(&app_dir())
}

pub fn mf_camera_dir() -> PathBuf {
    mf_camera_dir_in(&app_dir())
}

/// A file's SHA-256 as lower-case hex, read in pieces, or None when it can't be read (_sha256).
pub fn sha256_file(path: &Path) -> Option<String> {
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

/// The PowerShell that runs the admin script: Windows' own, from its system folder rather than PATH (_powershell).
pub fn powershell_path(system_dir: Option<&Path>) -> String {
    match system_dir {
        Some(dir) => dir.join("WindowsPowerShell").join("v1.0").join("powershell.exe").to_string_lossy().into_owned(),
        None => "powershell".into(),
    }
}

/// Starts command with nothing attached and waits for it, killing it past timeout (subprocess.run's timeout).
#[cfg(any(windows, test))]
fn run_process(mut command: std::process::Command, timeout: Duration) -> Result<i32, RunFailure> {
    use std::process::Stdio;
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(RunFailure::Failed)?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(RunFailure::Failed)? {
            return Ok(status.code().unwrap_or(-1));
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunFailure::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
pub use real::Windows;

#[cfg(windows)]
mod real {
    use super::*;
    use crate::wincam;
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// The real thing: this computer's registry, disk and network.
    pub struct Windows;

    impl InstallSys for Windows {
        fn mf_installable(&self) -> bool {
            wincam::mf_installable(&wincam::System, &self.mf_dir())
        }
        fn mf_registered(&self) -> bool {
            wincam::mf_registered(&wincam::System, 0)
        }
        fn camera_access_off(&self) -> bool {
            wincam::camera_access_off(&wincam::System)
        }
        fn mf_dir(&self) -> PathBuf {
            mf_camera_dir()
        }
        fn uc_dir(&self) -> PathBuf {
            unitycapture_dir()
        }
        fn uc_user_folder(&self) -> Option<PathBuf> {
            // Telescope before 3.1.2 registered it from beside the app; the script only touches a copy that checks out
            let (_, folder) = wincam::uc_registration(&wincam::System, &self.uc_dir())?;
            (folder != wincam::protected_dir(&wincam::System, "UnityCapture")).then_some(folder)
        }
        fn system_dir(&self) -> Option<PathBuf> {
            crate::win::system_dir()
        }
        fn open_url(&self, url: &str, timeout: Duration) -> std::io::Result<Box<dyn Read>> {
            let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into();
            let response = agent.get(url).call().map_err(std::io::Error::other)?;
            Ok(Box::new(response.into_body().into_reader()))
        }
        fn run(&self, program: &str, args: &[String], timeout: Duration) -> Result<i32, RunFailure> {
            let mut command = std::process::Command::new(program);
            command.args(args).creation_flags(CREATE_NO_WINDOW);
            run_process(command, timeout)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// What the network does for one file.
    enum Serve {
        Bytes(Vec<u8>),
        /// Never gets as far as answering, like OSError("offline").
        Refuse,
        /// Starts answering, then the connection goes.
        CutOff,
    }

    struct Run {
        program: String,
        args: Vec<String>,
        timeout: Duration,
    }

    /// Stands in for Windows and the network only: the disk, the hashing and the messages are the real ones.
    struct Fake {
        mf_installable: bool,
        mf_registered: bool,
        access_off: bool,
        app: PathBuf,
        system: Option<PathBuf>,
        served: HashMap<String, Serve>,
        exits: RefCell<Vec<Result<i32, RunFailure>>>,
        runs: RefCell<Vec<Run>>,
        fetches: RefCell<Vec<(String, Duration)>>,
    }

    fn fake() -> Fake {
        Fake {
            mf_installable: false,
            mf_registered: false,
            access_off: false,
            app: telescope_core::testing::scratch_dir("wininstall"),
            system: None,
            served: HashMap::new(),
            exits: RefCell::new(Vec::new()),
            runs: RefCell::new(Vec::new()),
            fetches: RefCell::new(Vec::new()),
        }
    }

    /// Windows 11 with the camera's DLL beside the app.
    fn windows_11(exits: Vec<Result<i32, RunFailure>>) -> Fake {
        let fake = Fake { mf_installable: true, exits: RefCell::new(exits), served: good_downloads(), ..fake() };
        std::fs::create_dir_all(fake.mf_dir()).unwrap();
        std::fs::write(fake.mf_dir().join(MF_DLL), b"the camera").unwrap();
        fake
    }

    /// Reads what it has, then the connection goes.
    struct Cut(bool);

    impl Read for Cut {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if std::mem::replace(&mut self.0, true) {
                return Err(std::io::Error::other("connection reset"));
            }
            buf[..4].copy_from_slice(b"half");
            Ok(4)
        }
    }

    impl InstallSys for Fake {
        fn mf_installable(&self) -> bool { self.mf_installable }
        fn mf_registered(&self) -> bool { self.mf_registered }
        fn camera_access_off(&self) -> bool { self.access_off }
        fn mf_dir(&self) -> PathBuf { self.app.join("virtualcamera") }
        fn uc_dir(&self) -> PathBuf { self.app.join("unitycapture") }
        fn uc_user_folder(&self) -> Option<PathBuf> { None }
        fn uc_files(&self) -> Vec<(String, String)> {
            UC_SHA256.iter().map(|(n, _)| (n.to_string(), hash(n.as_bytes()))).collect()
        }
        fn system_dir(&self) -> Option<PathBuf> { self.system.clone() }
        fn open_url(&self, url: &str, timeout: Duration) -> std::io::Result<Box<dyn Read>> {
            let name = url.rsplit('/').next().unwrap();
            self.fetches.borrow_mut().push((name.to_string(), timeout));
            match self.served.get(name) {
                Some(Serve::Bytes(data)) => Ok(Box::new(std::io::Cursor::new(data.clone()))),
                Some(Serve::CutOff) => Ok(Box::new(Cut(false))),
                _ => Err(std::io::Error::other("offline")),
            }
        }
        fn run(&self, program: &str, args: &[String], timeout: Duration) -> Result<i32, RunFailure> {
            self.runs.borrow_mut().push(Run { program: program.to_string(), args: args.to_vec(), timeout });
            let mut exits = self.exits.borrow_mut();
            if exits.is_empty() { Ok(0) } else { exits.remove(0) }
        }
    }

    fn hash(data: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, data).as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Each file served as its own name, which is what Fake's uc_files hashes.
    fn good_downloads() -> HashMap<String, Serve> {
        UC_SHA256.iter().map(|(n, _)| (n.to_string(), Serve::Bytes(n.as_bytes().to_vec()))).collect()
    }

    fn both_dlls_there(fake: &Fake) {
        std::fs::create_dir_all(fake.uc_dir()).unwrap();
        for (name, _) in UC_SHA256 {
            std::fs::write(fake.uc_dir().join(name), name).unwrap();
        }
    }

    fn with_dlls(fake: Fake) -> Fake {
        both_dlls_there(&fake);
        fake
    }

    fn unbase64(text: &str) -> Vec<u8> {
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        for chunk in text.as_bytes().chunks(4) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, c)| {
                n | (TABLE.iter().position(|t| t == c).unwrap_or(0) as u32) << (18 - 6 * i)
            });
            let kept = chunk.iter().filter(|&&c| c != b'=').count() - 1;
            out.extend_from_slice(&n.to_be_bytes()[1..1 + kept]);
        }
        out
    }

    /// The script that reaches the admin PowerShell, out of the command line (_elevated_script).
    fn elevated_script(run: &Run) -> String {
        let ps = run.args.last().unwrap();
        let marker = "'-EncodedCommand', '";
        let start = ps.find(marker).unwrap() + marker.len();
        let bytes = unbase64(ps[start..].split('\'').next().unwrap());
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn windows_11_installs_its_own_camera_from_program_files() {
        let fake = windows_11(vec![]);
        assert_eq!(install_camera(&fake, 4, false, &|_| {}), (true, "Installed".into()));
        let script = elevated_script(&fake.runs.borrow()[0]);
        assert!(script.contains("$dst = Join-Path $telescope 'VirtualCamera'"));
        let digest = hash(b"the camera").to_uppercase();
        assert!(script.contains(&format!("$files = [ordered]@{{ 'TelescopeCamera.dll' = '{digest}' }}")), "hashed before the prompt, checked after");
        let (copy, check) = (script.find("Copy-Item").unwrap(), script.rfind("Get-Sha256 $target").unwrap());
        let register = script.find("'/s', '/n', '/i:4'").unwrap();
        let unregister_uc = script.find("'UnityCaptureFilter64.dll'").unwrap();
        assert!(script.find("Test-AdminOnly $telescope").unwrap() < copy);
        assert!(copy < check && check < register && register < unregister_uc);
        assert!(script[unregister_uc..].contains("'/u'"), "the old camera goes only once the new one is in");
        assert!(fake.fetches.borrow().is_empty(), "nothing downloaded");
    }

    #[test]
    fn a_refused_windows_11_camera_falls_back_to_unitycapture_unless_its_cameras_are_still_there() {
        let fake = windows_11(vec![Ok(EXIT_REGSVR32)]);
        assert_eq!(install_camera(&fake, 1, false, &|_| {}), (true, "Installed".into()));
        let fetched: Vec<String> = fake.fetches.borrow().iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(fetched, ["UnityCaptureFilter32.dll", "UnityCaptureFilter64.dll"]);
        let runs = fake.runs.borrow();
        assert_eq!(runs.len(), 2);
        let script = elevated_script(&runs[1]);
        assert!(script.contains("\"/i:UnityCaptureName=Telescope\""));
        assert!(!script.contains("UnityCaptureDevices") && !script.contains("'/u'"), "one camera, not reset");

        let cancelled = windows_11(vec![Ok(1223)]);
        assert_eq!(install_camera(&cancelled, 1, false, &|_| {}), (false, "Registration failed (cancelled or denied?)".into()));
    }

    // test_platform_windows.py::test_cameras_windows_wouldnt_change_stay_without_unitycapture_on_top
    #[test]
    fn cameras_windows_wouldnt_change_stay_without_unitycapture_on_top() {
        for access_off in [true, false] {
            let kept = Fake { mf_registered: true, access_off, ..windows_11(vec![Ok(EXIT_REGSVR32)]) };
            let (ok, msg) = install_camera(&kept, 1, true, &|_| {});
            assert!(!ok);
            assert_eq!(msg.contains("Camera access is off"), access_off, "{msg}");
            assert_eq!(msg != "Windows refused to register the driver (regsvr32 failed)", access_off, "{msg}");
            assert_eq!(kept.runs.borrow().len(), 1, "UnityCapture would list Telescope twice");
            assert!(kept.fetches.borrow().is_empty());
        }
    }

    #[test]
    fn a_download_that_doesnt_check_out_is_never_registered() {
        let mut served = good_downloads();
        served.insert("UnityCaptureFilter64.dll".into(), Serve::Bytes(b"tampered".to_vec()));
        let fake = Fake { served, ..fake() };
        let (ok, msg) = install_camera(&fake, 1, false, &|_| {});
        assert!(!ok);
        assert_eq!(msg, format!("UnityCaptureFilter64.dll failed checksum verification (got {}...) - not registering", &hash(b"tampered")[..12]));
        assert!(fake.runs.borrow().is_empty());
        assert!(!fake.uc_dir().join("UnityCaptureFilter64.dll.part").exists(), "no part file left");
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

    /// Where the pieces of the Windows 11 camera sit in the Python app's virtualcamera folder.
    fn virtualcamera() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/virtualcamera")
    }

    // test_platform_windows.py::test_unitycapture_dir_frozen_and_source
    #[test]
    fn unitycapture_dir_frozen_and_source() {
        assert_eq!(unitycapture_dir_in(Path::new("/app")), Path::new("/app").join("unitycapture"));
        assert_eq!(unitycapture_dir(), std::env::current_exe().unwrap().parent().unwrap().join("unitycapture"));
    }

    // test_platform_windows.py::test_mf_camera_dir_frozen_and_source
    #[test]
    fn mf_camera_dir_frozen_and_source() {
        assert_eq!(mf_camera_dir_in(Path::new("/app")), Path::new("/app").join("virtualcamera"));
        assert_eq!(mf_camera_dir(), std::env::current_exe().unwrap().parent().unwrap().join("virtualcamera"));
    }

    // test_platform_windows.py::test_sha256_reads_entire_file
    #[test]
    fn sha256_reads_entire_file() {
        let dir = telescope_core::testing::scratch_dir("sha");
        let data = vec![b'a'; (1 << 20) + 17];
        std::fs::write(dir.join("large.bin"), &data).unwrap();
        assert_eq!(sha256_file(&dir.join("large.bin")), Some(hash(&data)));
        assert_eq!(sha256_file(&dir.join("missing.bin")), None);
    }

    // test_platform_windows.py::test_download_unitycapture_verifies_both_files
    #[test]
    fn download_unitycapture_verifies_both_files() {
        let fake = Fake { served: good_downloads(), ..fake() };
        let progress = RefCell::new(Vec::new());
        assert_eq!(download_unitycapture(&fake, &|m| progress.borrow_mut().push(m.to_string())), (true, "Downloaded".into()));
        assert_eq!(*progress.borrow(), ["Downloading UnityCaptureFilter32.dll...", "Downloading UnityCaptureFilter64.dll..."]);
        for (name, _) in UC_SHA256 {
            assert_eq!(std::fs::read(fake.uc_dir().join(name)).unwrap(), name.as_bytes(), "the bytes that were served");
        }
        assert_eq!(*fake.fetches.borrow(), [("UnityCaptureFilter32.dll".to_string(), DOWNLOAD_TIMEOUT), ("UnityCaptureFilter64.dll".to_string(), DOWNLOAD_TIMEOUT)]);
        assert_eq!(DOWNLOAD_TIMEOUT, Duration::from_secs(30));
        assert_eq!(std::fs::read_dir(fake.uc_dir()).unwrap().count(), 2, "no part files left");
    }

    // test_platform_windows.py::test_download_unitycapture_handles_network_and_checksum_failures
    #[test]
    fn download_unitycapture_handles_network_and_checksum_failures() {
        let offline = Fake { served: UC_SHA256.iter().map(|(n, _)| (n.to_string(), Serve::Refuse)).collect(), ..fake() };
        let (ok, msg) = download_unitycapture(&offline, &|_| {});
        assert!(!ok);
        assert_eq!(msg, "Download failed: offline");

        let wrong = Fake { served: UC_SHA256.iter().map(|(n, _)| (n.to_string(), Serve::Bytes(b"wrong".to_vec()))).collect(), ..fake() };
        let (ok, msg) = download_unitycapture(&wrong, &|_| {});
        assert!(!ok && msg.contains("checksum"), "{msg}");
        assert!(msg.contains(&hash(b"wrong")[..12]), "{msg}");
        assert!(!wrong.uc_dir().join("UnityCaptureFilter32.dll").exists());
        assert_eq!(std::fs::read_dir(wrong.uc_dir()).unwrap().count(), 0, "nothing is left behind");
    }

    // test_platform_windows.py::test_a_cut_off_download_leaves_nothing_for_the_next_try_to_trip_over
    #[test]
    fn a_cut_off_download_leaves_nothing_for_the_next_try_to_trip_over() {
        let cut = Fake { served: UC_SHA256.iter().map(|(n, _)| (n.to_string(), Serve::CutOff)).collect(), ..fake() };
        let (ok, msg) = download_unitycapture(&cut, &|_| {});
        assert!(!ok && msg.contains("connection reset"), "{msg}");
        assert_eq!(std::fs::read_dir(cut.uc_dir()).unwrap().count(), 0);
    }

    // test_platform_windows.py::test_going_back_to_one_camera_unregisters_them_all_first
    #[test]
    fn going_back_to_one_camera_unregisters_them_all_first() {
        let fake = with_dlls(fake());
        assert!(register_unitycapture(&fake, 1, false).0);
        assert!(register_unitycapture(&fake, 1, true).0);
        let runs = fake.runs.borrow();
        assert!(!elevated_script(&runs[0]).contains("'/u'"));
        let script = elevated_script(&runs[1]);
        assert!(script.find("'/s', '/u', $dll").unwrap() < script.find("UnityCaptureName").unwrap());
        assert!(!script.contains("UnityCaptureDevices"));
    }

    // test_platform_windows.py::test_registering_more_cameras_asks_unitycapture_for_them
    #[test]
    fn registering_more_cameras_asks_unitycapture_for_them() {
        let fake = with_dlls(fake());
        register_unitycapture(&fake, 1, false);
        register_unitycapture(&fake, 4, false);
        let runs = fake.runs.borrow();
        assert!(!elevated_script(&runs[0]).contains("UnityCaptureDevices"));
        assert!(elevated_script(&runs[1]).contains("'/s', '/i:UnityCaptureDevices=4', '\"/i:UnityCaptureName=Telescope\"'"));
    }

    // test_platform_windows.py::test_register_invokes_elevated_powershell
    #[test]
    fn register_invokes_elevated_powershell() {
        // Run again as a child with Program Files pointed away: a test can't safely change its own process's variables
        const CHILD: &str = "TELESCOPE_TEST_REDIRECTED_PROGRAM_FILES";
        let redirected = "redirected-program-files";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "wininstall::tests::register_invokes_elevated_powershell"])
                .env(CHILD, "1").env("ProgramFiles", redirected).env("ProgramW6432", redirected).status().unwrap();
            assert!(status.success());
            return;
        }
        assert_eq!(std::env::var("ProgramFiles").as_deref(), Ok(redirected));
        let fake = with_dlls(fake());
        assert_eq!(register_unitycapture(&fake, 1, false), (true, "Installed".into()));
        let runs = fake.runs.borrow();
        let run = &runs[0];
        assert_eq!(run.program, powershell_path(None));
        assert_eq!(run.args[..2], ["-NoProfile", "-Command"]);
        let ps = run.args.last().unwrap();
        assert!(ps.contains("-Verb RunAs") && ps.contains("exit $p.ExitCode"));
        assert!(ps.contains("[Environment]::SystemDirectory"), "the real PowerShell, not one found on PATH");
        assert_eq!(run.timeout, Duration::from_secs(60));
        let script = elevated_script(run);
        // The copy, the hash check that counts, and the registration all happen as admin, on the protected copy
        assert!(!script.contains(redirected));
        assert!(script.contains("[Environment]::GetFolderPath('ProgramFiles')"));
        // A tree that's already there is checked before anything touches it, and never repaired
        let first_check = script.find("-not (Test-AdminOnly $telescope)").unwrap();
        let (new_item, copy) = (script.find("New-Item").unwrap(), script.find("Copy-Item").unwrap());
        assert!(first_check < new_item && new_item < copy);
        assert!(!script.contains("/reset") && !script.contains("/T"));
        // No cmdlet that needs a module loaded: started from PowerShell 7, Windows PowerShell can't load them
        assert!(!script.contains("Get-Acl -") && !script.contains("Get-FileHash -") && script.contains("PSModulePath"));
        // ...and the finished tree is checked again before the hashes that count and the registration
        let last_check = script.rfind("-not (Test-AdminOnly $telescope)").unwrap();
        let last_hash = script.rfind("Get-Sha256 $target").unwrap();
        assert!(copy < last_check && last_check < last_hash && last_hash < script.find("Start-Process $regsvr32").unwrap());
        assert!(script.contains("Join-Path ([Environment]::SystemDirectory) 'regsvr32.exe'"));
        let register = script.find("regsvr32").unwrap();
        assert!(copy < last_hash && last_hash < register);
        assert!(script[register - 200..].contains("Join-Path $dst $name"));
        for (_, digest) in fake.uc_files() {
            assert!(script.contains(&digest));
        }
        assert_eq!(script.matches("\"/i:UnityCaptureName=Telescope\"").count(), 1, "once, inside the loop over both DLLs");
    }

    // test_platform_windows.py::test_register_unitycapture_survives_an_apostrophe_in_the_folder
    #[test]
    fn register_unitycapture_survives_an_apostrophe_in_the_folder() {
        let fake = fake();
        let fake = with_dlls(Fake { app: fake.app.join("O'Brien"), ..fake });
        register_unitycapture(&fake, 1, false);
        let script = elevated_script(&fake.runs.borrow()[0]);
        let src_line = script.lines().find(|line| line.starts_with("$src = ")).unwrap();
        assert_eq!(src_line, format!("$src = '{}'", fake.uc_dir().to_string_lossy().replace('\'', "''")));
    }

    // test_platform_windows.py::test_register_drops_a_tampered_file_so_a_retry_downloads_it_again
    #[test]
    fn register_drops_a_tampered_file_so_a_retry_downloads_it_again() {
        let fake = fake();
        std::fs::create_dir_all(fake.uc_dir()).unwrap();
        let path = fake.uc_dir().join("UnityCaptureFilter32.dll");
        std::fs::write(&path, "half").unwrap();
        assert!(!register_unitycapture(&fake, 1, false).0);
        assert!(!path.exists());
    }

    // test_platform_windows.py::test_register_refuses_missing_or_tampered_files
    #[test]
    fn register_refuses_missing_or_tampered_files() {
        let fake = fake();
        let (ok, msg) = register_unitycapture(&fake, 1, false);
        assert!(!ok && msg.contains("checksum"), "{msg}");
        assert!(fake.runs.borrow().is_empty());
    }

    // test_platform_windows.py::test_register_names_what_failed_as_admin
    #[test]
    fn register_names_what_failed_as_admin() {
        for (code, words) in [(2, "checksum"), (3, "regsvr32"), (4, "delete that folder"), (5, "hit an error")] {
            let fake = with_dlls(Fake { exits: RefCell::new(vec![Ok(code)]), ..fake() });
            let (ok, msg) = register_unitycapture(&fake, 1, false);
            assert!(!ok && msg.contains(words), "{code}: {msg}");
        }
    }

    // test_platform_windows.py::test_register_normalizes_process_failures
    #[test]
    fn register_normalizes_process_failures() {
        let cases: Vec<(Result<i32, RunFailure>, &str)> = vec![
            (Ok(1), "Registration failed (cancelled or denied?)"),
            (Err(RunFailure::TimedOut), "Timed out"),
            (Err(RunFailure::Failed(std::io::Error::other("powershell missing"))), "powershell missing"),
        ];
        for (effect, expected) in cases {
            let fake = with_dlls(Fake { exits: RefCell::new(vec![effect]), ..fake() });
            assert_eq!(register_unitycapture(&fake, 1, false), (false, expected.to_string()));
        }
    }

    // What subprocess.run does for _run_as_admin, on a real process
    #[cfg(unix)]
    #[test]
    fn the_real_process_runner_reports_codes_timeouts_and_failures_to_start() {
        let mut exits = std::process::Command::new("sh");
        exits.args(["-c", "exit 7"]);
        assert!(matches!(run_process(exits, Duration::from_secs(10)), Ok(7)));
        let mut sleeps = std::process::Command::new("sleep");
        sleeps.arg("30");
        let started = std::time::Instant::now();
        assert!(matches!(run_process(sleeps, Duration::from_millis(100)), Err(RunFailure::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(10));
        let missing = std::process::Command::new("/nonexistent/powershell");
        assert!(matches!(run_process(missing, Duration::from_secs(1)), Err(RunFailure::Failed(_))));
    }

    // test_platform_windows.py::test_the_first_powershell_comes_from_the_system_folder
    #[test]
    fn the_first_powershell_comes_from_the_system_folder() {
        let system = Path::new("C:/Windows/System32");
        let fake = with_dlls(Fake { system: Some(system.to_path_buf()), ..fake() });
        register_unitycapture(&fake, 1, false);
        assert_eq!(fake.runs.borrow()[0].program, system.join("WindowsPowerShell").join("v1.0").join("powershell.exe").to_string_lossy());
        assert_eq!(powershell_path(None), "powershell");
    }

    // test_platform_windows.py::test_a_cancelled_or_unsafe_install_doesnt_ask_again_for_unitycapture
    #[test]
    fn a_cancelled_or_unsafe_install_doesnt_ask_again_for_unitycapture() {
        let cases: Vec<(Result<i32, RunFailure>, String)> = vec![
            (Ok(1), "Registration failed (cancelled or denied?)".into()),
            (Err(RunFailure::Failed(std::io::Error::other("denied"))), "denied".into()),
            (Ok(EXIT_UNSAFE_FOLDER), admin_result(EXIT_UNSAFE_FOLDER).1),
        ];
        for (effect, expected) in cases {
            let fake = windows_11(vec![effect]);
            assert_eq!(install_camera(&fake, 1, false, &|_| {}), (false, expected.clone()));
            assert_eq!(fake.runs.borrow().len(), 1, "{expected}: UnityCapture isn't tried");
            assert!(fake.fetches.borrow().is_empty(), "{expected}: and nothing is downloaded");
        }
    }

    // test_platform_windows.py::test_windows_10_installs_unitycapture
    #[test]
    fn windows_10_installs_unitycapture() {
        let fake = Fake { served: good_downloads(), ..fake() };
        let progress = RefCell::new(Vec::new());
        assert_eq!(install_camera(&fake, 1, true, &|m| progress.borrow_mut().push(m.to_string())), (true, "Installed".into()));
        assert_eq!(*progress.borrow(), ["Downloading driver files...", "Registering (admin access required)..."], "each file isn't named here");
        assert_eq!(fake.fetches.borrow().len(), 2);
        let runs = fake.runs.borrow();
        assert_eq!(runs.len(), 1);
        assert!(elevated_script(&runs[0]).contains("'/s', '/u', $dll"), "going back to one camera unregisters first");
    }

    // test_platform_windows.py::test_windows_11_installs_its_own_camera_and_never_downloads_unitycapture
    #[test]
    fn windows_11_installs_its_own_camera_and_never_downloads_unitycapture() {
        let fake = windows_11(vec![]);
        let progress = RefCell::new(Vec::new());
        assert_eq!(install_camera(&fake, 4, false, &|m| progress.borrow_mut().push(m.to_string())), (true, "Installed".into()));
        assert_eq!(*progress.borrow(), ["Registering (admin access required)..."]);
        assert!(fake.fetches.borrow().is_empty(), "nothing downloaded");
        let runs = fake.runs.borrow();
        assert_eq!(runs.len(), 1);
        assert!(elevated_script(&runs[0]).contains("'/s', '/n', '/i:4'"));
    }

    // test_platform_windows.py::test_an_update_renames_a_loaded_dll_out_of_the_way
    #[test]
    fn an_update_renames_a_loaded_dll_out_of_the_way() {
        let script = mf_install_script(Path::new("C:/app"), &"AB".repeat(32), 1, None);
        let rename = script.find("Move-Item -LiteralPath $target").unwrap();
        assert!(script.find("'*.old'").unwrap() < rename && rename < script.find("Copy-Item").unwrap());
    }

    // test_platform_windows.py::test_unitycapture_registered_from_the_app_folder_goes_through_a_checked_copy
    #[test]
    fn unitycapture_registered_from_the_app_folder_goes_through_a_checked_copy() {
        let script = mf_install_script(Path::new("C:/app"), &"AB".repeat(32), 1, Some(Path::new("C:/app/unitycapture")));
        let (copy, check) = (script.find("Copy-Item -LiteralPath $from").unwrap(), script.find("Get-Sha256 $copy").unwrap());
        assert!(script.find("'/n', '/i:1'").unwrap() < copy && copy < check && check < script.rfind("'/u'").unwrap());
        assert!(script.contains(UC_SHA256[1].1));
        assert!(!mf_install_script(Path::new("C:/app"), &"AB".repeat(32), 1, None).contains("$from"));
    }

    // test_platform_windows.py::test_the_clsids_and_version_match_the_dll
    #[test]
    fn the_clsids_and_version_match_the_dll() {
        let dllmain = std::fs::read_to_string(virtualcamera().join("dllmain.cpp")).unwrap();
        for clsid in crate::wincam::MF_CLSIDS {
            let parts: Vec<&str> = clsid.trim_matches(['{', '}']).split('-').collect();
            let line = format!("{{0x{}, 0x{}, 0x{}, {{0x{}, 0x{}", parts[0], parts[1], parts[2], &parts[3][..2], &parts[3][2..]);
            assert!(dllmain.contains(&line), "{clsid} isn't in dllmain.cpp");
        }
        let camera = std::fs::read_to_string(virtualcamera().join("camera.h")).unwrap();
        assert!(camera.contains(&format!("kCameraVersion = {};", crate::wincam::MF_VERSION)));
    }
}
