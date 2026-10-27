#!/usr/bin/env python3
"""Go through the Windows parts of the release checklist on a Windows machine reached over SSH, and print pass, fail or
skipped for each.

Usage: windows_pass.py downloads/n932 --ssh "-p 2222 user@host" [--previous downloads/n838] [--agree-adb-licence]
                       [--only NAME ...] [--list]

Use a throwaway Windows 11 VM (snapshot it first): the run uninstalls Telescope, deletes its settings and turns off the
UAC prompt while it runs (it counts elevations from the security log instead, then puts the prompt back). The SSH
account has to be an administrator that is also signed in on the desktop, since the app gets driven in that session.
"""

import argparse
import base64
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
WORK = r"C:\TelescopeTest"
TASK = "TelescopeTestJob"
PLUGIN = REPO / "desktop" / "examples" / "plugins" / "black_and_white"

CHECKS = []


class Fail(Exception):
    pass


class Skip(Exception):
    pass


def check(fn):
    CHECKS.append(fn)
    return fn


def need(cond, msg):
    if not cond:
        raise Fail(msg)


# ── Talking to Windows ────────────────────────────────────────────────────────

class Windows:
    def __init__(self, ssh_args: str):
        self.ssh = ["ssh", "-o", "BatchMode=yes", *shlex.split(ssh_args)]

    def _powershell(self, command: str, timeout=600) -> subprocess.CompletedProcess:
        encoded = base64.b64encode(command.encode("utf-16-le")).decode()  # survives cmd or PowerShell as the SSH shell
        return subprocess.run([*self.ssh, f"powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {encoded}"],
                              stdin=subprocess.DEVNULL, capture_output=True, timeout=timeout)

    def put(self, data: bytes, remote: str):
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(data)
        try:
            self.copy(Path(f.name), remote)
        finally:
            os.unlink(f.name)

    def copy(self, local: Path, remote: str):
        # scp takes the same options as ssh, except the port is -P there
        args = ["-P" if x == "-p" else x for x in self.ssh[1:-1]]
        folder, _, _ = remote.rpartition("\\")
        self._powershell(f"$null = New-Item -ItemType Directory -Force '{folder}'")
        r = subprocess.run(["scp", "-q", *args, str(local), f"{self.ssh[-1]}:" + remote.replace("\\", "/")],
                           stdin=subprocess.DEVNULL, capture_output=True, text=True)
        if r.returncode:
            raise RuntimeError(f"couldn't copy {local.name}: {r.stderr.strip()}")

    def admin(self, script: str, timeout=600) -> str:
        """Run PowerShell as the SSH account (an elevated administrator) and return what it printed."""
        self.put(script.encode("utf-8-sig"), rf"{WORK}\admin.ps1")
        r = self._powershell(rf"[Console]::OutputEncoding = [Text.Encoding]::UTF8; & '{WORK}\admin.ps1' *>&1 | Out-String -Width 400",
                             timeout=timeout)
        out = r.stdout.decode("utf-8", errors="replace")
        if r.returncode:
            raise RuntimeError(f"PowerShell failed: {out}{r.stderr.decode(errors='replace')}".strip())
        return out.strip()

    def desktop(self, script: str, timeout=300) -> str:
        """Run PowerShell on the signed-in user's desktop, unelevated, with windows/ui.ps1 loaded."""
        self.put(script.encode("utf-8-sig"), rf"{WORK}\body.ps1")
        out = self.admin(rf"""
$job = '{WORK}\job.ps1'
Set-Content $job -Encoding UTF8 @'
try {{ . '{WORK}\ui.ps1'; . '{WORK}\body.ps1' *>&1 | Out-String -Width 400 | Set-Content '{WORK}\job.out' -Encoding UTF8 }}
catch {{ "JOB FAILED: $_ (line $($_.InvocationInfo.ScriptLineNumber))" | Add-Content '{WORK}\job.out' -Encoding UTF8 }}
Set-Content '{WORK}\job.done' 1
'@
Remove-Item '{WORK}\job.out', '{WORK}\job.done' -ErrorAction SilentlyContinue
$user = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $user) {{ throw "nobody is signed in on the desktop" }}
$action = New-ScheduledTaskAction -Execute powershell.exe -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File $job"
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds {timeout}) -AllowStartIfOnBatteries
Register-ScheduledTask {TASK} -Action $action -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask {TASK}
$end = (Get-Date).AddSeconds({timeout + 15})
while (-not (Test-Path '{WORK}\job.done') -and (Get-Date) -lt $end) {{ Start-Sleep -Milliseconds 300 }}
Unregister-ScheduledTask {TASK} -Confirm:$false
if (-not (Test-Path '{WORK}\job.done')) {{ "JOB FAILED: timed out after {timeout}s" }}
Get-Content '{WORK}\job.out' -Raw -ErrorAction SilentlyContinue
""", timeout=timeout + 120)  # room for the job's own timeout to be reported, even on a busy machine
        if "JOB FAILED:" in out:
            raise Fail(out[out.index("JOB FAILED:") + 12:].strip())
        return out


def ps(src: str, **values) -> str:
    """PowerShell with <<name>> replaced by values quoted as PowerShell strings, so the braces can stay as they are."""
    for k, v in values.items():
        src = src.replace(f"<<{k}>>", "'" + str(v).replace("'", "''") + "'")
    return src


APP = r"$env:LOCALAPPDATA\Programs\Telescope"
MENU = r"$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Telescope.lnk"
UNINSTALL_KEY = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{BF097F77-F8B9-4F70-B23C-0B500DB1A07C}_is1"
UAC_KEY = r"HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System"
PROCESS_AUDIT = "{0CCE922B-69AE-11D9-BED3-505054503030}"  # the Process Creation subcategory, whatever the language

# Everything Telescope leaves on a machine, so a check can start from a computer that never had it
CLEAN = r"""
Get-Process TelescopeDesktop*, TelescopeSetup*, adb, unins*, _iu* -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 1
$app = "$env:LOCALAPPDATA\Programs\Telescope"
if (Test-Path "$app\unins000.exe") {
    Start-Process "$app\unins000.exe" '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait
    for ($i = 0; $i -lt 60 -and (Test-Path "$app\unins000.exe"); $i++) { Start-Sleep 1 }
}
foreach ($view in 'HKLM:\SOFTWARE\Classes', 'HKLM:\SOFTWARE\WOW6432Node\Classes') {
    Get-ChildItem "$view\CLSID" -ErrorAction SilentlyContinue | Where-Object { $_.PSChildName -like '{5C2CD55C-92AD-4999-8666-912BD3E7*' } |
        Remove-Item -Recurse -Force
    Get-ChildItem "$view\CLSID\{860BB310-5D01-11d0-BD3B-00A0C911CE86}\Instance" -ErrorAction SilentlyContinue |
        Where-Object { $_.PSChildName -like '{5C2CD55C-92AD-4999-8666-912BD3E7*' } | Remove-Item -Recurse -Force
}
# The Windows 11 camera: its cameras go with its registration (Frame Server keeps the DLL loaded, so the folder may stay)
$vcam = "$env:ProgramFiles\Telescope\VirtualCamera\TelescopeCamera.dll"
if (Test-Path $vcam) { Start-Process "$env:windir\System32\regsvr32.exe" '/s', '/u', "`"$vcam`"" -Wait }
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue "$env:ProgramFiles\Telescope", $app, "$env:APPDATA\Telescope",
    "$env:LOCALAPPDATA\Telescope", "$env:TEMP\Telescope", "$HOME\Downloads\Telescope*", "<<uninstall>>"
Remove-ItemProperty HKCU:\Software\Microsoft\Windows\CurrentVersion\Run Telescope -ErrorAction SilentlyContinue
Get-NetFirewallRule | Where-Object DisplayName -like '*telescope*' | Remove-NetFirewallRule
""".replace("<<uninstall>>", UNINSTALL_KEY)

# The cameras apps see: what DirectShow lists UnityCapture under, 64 and 32-bit, and the Windows 11 camera's ("mf")
CAMERAS = r"""
foreach ($view in 'HKLM:\SOFTWARE\Classes', 'HKLM:\SOFTWARE\WOW6432Node\Classes') {
    $bits = if ($view -like '*WOW6432Node*') { 32 } else { 64 }
    Get-ChildItem "$view\CLSID\{860BB310-5D01-11d0-BD3B-00A0C911CE86}\Instance" -ErrorAction SilentlyContinue |
        ForEach-Object { "$bits " + (Get-ItemProperty $_.PSPath).FriendlyName }
}
# The Windows 11 camera (desktop/virtualcamera): a software device per camera, named in its device interface (Windows
# adds " (Windows Virtual Camera)" to the name it was given)
Get-ChildItem 'HKLM:\SYSTEM\CurrentControlSet\Control\DeviceClasses\{e5323777-f976-4f5b-9b55-b94699c46e44}' -ErrorAction SilentlyContinue |
    Where-Object PSChildName -like '*VCAMDEVAPI*' | ForEach-Object { Get-ChildItem -LiteralPath $_.PSPath -ErrorAction SilentlyContinue } |
    ForEach-Object { "mf " + ((Get-ItemProperty -LiteralPath "$($_.PSPath)\Device Parameters" -ErrorAction SilentlyContinue).FriendlyName -replace ' \(Windows Virtual Camera\)$') }
"""

# The Windows build: from 22000 (Windows 11) Telescope installs its own camera rather than UnityCapture
WINDOWS_11 = 22000


def clean(a):
    a.win.admin(CLEAN)


def windows_11(a) -> bool:
    if a.build_number is None:
        a.build_number = int(a.win.admin("[Environment]::OSVersion.Version.Build"))
    return a.build_number >= WINDOWS_11


def win_now(a) -> str:
    return a.win.admin("(Get-Date).ToString('o')")


def elevations(a, since: str) -> list:
    """Admin prompts since then that Telescope, its setup or its uninstaller caused, as "powershell.exe from
    TelescopeDesktop.exe": processes that started elevated from one that wasn't, with one of those up their tree."""
    out = a.win.admin(ps(r"""
$since = [DateTime]::Parse(<<since>>)
$procs = @{}  # process id: its starts, newest first (Windows reuses ids)
$all = foreach ($e in Get-WinEvent -FilterHashtable @{LogName = 'Security'; Id = 4688; StartTime = $since.AddMinutes(-30)} -ErrorAction SilentlyContinue) {
    $d = @{Time = $e.TimeCreated}; ([xml]$e.ToXml()).Event.EventData.Data | ForEach-Object { $d[$_.Name] = $_.'#text' }
    $procs[$d.NewProcessId] = @($procs[$d.NewProcessId] | Where-Object { $_ }) + $d
    $d
}
function Started($id, $before) { $procs[$id] | Where-Object { $_.Time -le $before } | Select-Object -First 1 }
foreach ($d in $all | Where-Object { $_.Time -ge $since -and $_.TokenElevationType -eq '%%1937' } | Sort-Object { $_.Time }) {
    $parent = Started $d.ProcessId $d.Time
    if ($parent -and $parent.TokenElevationType -ne '%%1938') { continue }  # started by something already elevated
    $names = @($d.ParentProcessName); $hop = $parent
    for ($i = 0; $hop -and $i -lt 10; $i++) { $names += $hop.ParentProcessName; $hop = Started $hop.ProcessId $hop.Time }
    $cause = $names | Where-Object { $_ -match '\\(TelescopeDesktop|TelescopeSetup|unins\d+|_unins|_iu\w+)\.(exe|tmp)$' } | Select-Object -First 1
    if ($cause) { "$(Split-Path -Leaf $d.NewProcessName) from $(Split-Path -Leaf $cause)" }
}""", since=since))
    return [line for line in out.splitlines() if line.strip()]


def upload(a, local: Path) -> str:
    """Copy a release file to the Windows machine once and return where it is there."""
    remote = rf"{WORK}\{local.parent.name}\{local.name}"
    size = a.win.admin(ps("if (Test-Path <<f>>) { (Get-Item <<f>>).Length }", f=remote))
    if size != str(local.stat().st_size):
        a.win.copy(local, remote)
    return remote


def watch_log(out: str) -> list:
    """Watch lines from a desktop job ("WATCH seconds|hwnd|pid|title|l,t,r,b|hung") as dicts."""
    rows = []
    for line in out.splitlines():
        if not line.startswith("WATCH "):
            continue
        f = line[6:].split("|")
        if f[2] == "gone":
            rows.append({"t": float(f[0]), "hwnd": f[1], "gone": True})
        else:
            l, t, r, b = map(int, f[4].split(","))
            rows.append({"t": float(f[0]), "hwnd": f[1], "pid": f[2], "title": f[3], "rect": (l, t, r, b),
                         "hung": f[5] == "1", "gone": False})
    return rows


def hung_seconds(rows: list) -> float:
    """The longest stretch any window was hung (Windows calls it Not Responding after about 5 s of that)."""
    longest, since = 0.0, {}
    for r in rows:
        if r.get("hung") and r["hwnd"] not in since:
            since[r["hwnd"]] = r["t"]
        elif not r.get("hung") and r["hwnd"] in since:
            longest = max(longest, r["t"] - since.pop(r["hwnd"]))
    end = rows[-1]["t"] if rows else 0
    return max([longest] + [end - t for t in since.values()])


def manifest(build: Path) -> dict:
    return json.loads((build / "manifest.json").read_text())


def installed(a) -> str:
    """The installed copy as "4.2.0 build 932", from its uninstall entry and its lib-<build> folder, or ""."""
    out = a.win.admin(ps(r"""
$v = (Get-ItemProperty <<k>> -ErrorAction SilentlyContinue).DisplayVersion
$b = Get-ChildItem "$env:LOCALAPPDATA\Programs\Telescope" -Directory -Filter "lib-*" -ErrorAction SilentlyContinue | Select-Object -First 1
if ($v -and $b) { "$v build $($b.Name.Substring(4))" }""", k=UNINSTALL_KEY))
    return out.strip()


def release(build: Path) -> str:
    m = manifest(build)
    return f"{m['version']} build {m['build']}"


def need_installed(a, build: Path, by: str):
    if installed(a) != release(build):
        raise Skip(f"{by} didn't leave {release(build)} installed")


# ── Checks ────────────────────────────────────────────────────────────────────
# Each takes the run's arguments, raises Fail or Skip, or returns a short note. They run in order and later ones lean
# on what earlier ones set up (an installed Telescope, its camera).

@check
def zip_launch(a):
    zip_path = upload(a, a.build / "Telescope-windows-portable.zip")
    a.win.admin("Get-Process TelescopeDesktop* -ErrorAction SilentlyContinue | Stop-Process -Force")
    out = a.win.desktop(ps(r"""
[Watch]::Start("TelescopeDesktop")
Open-AsUser (<<zip>> + "\TelescopeDesktop.exe")  # what a double-click on the exe in Explorer's view of the zip does
$run = Find-Control "Run" "Pane" 15 ""
if ($run) { Press $run }  # Windows asks whether to run it or extract everything first
Start-Sleep 20
$texts = @(Get-Texts) | Sort-Object -Unique
[Watch]::Stop() | ForEach-Object { "WATCH $_" }
$texts | ForEach-Object { "TEXT $_" }
Get-Process TelescopeDesktop* -ErrorAction SilentlyContinue | Stop-Process -Force
""", zip=zip_path), timeout=120)
    titles = sorted({r["title"] for r in watch_log(out) if not r["gone"]})
    texts = [line[5:] for line in out.splitlines() if line.startswith("TEXT ")]
    need("Extract Telescope first" in titles,
         f"no \"Extract Telescope first\" message, the windows were {titles or 'none'}: {' / '.join(texts)[:300]}")
    others = [t for t in titles if t != "Extract Telescope first"]
    need(not others, f"other windows came up too: {others}")


def first_start_time(a, more="") -> float:
    """Open the installed copy from its Start menu entry for the first time; the seconds until its window is ready."""
    out = a.win.desktop(ps(r"""
[Watch]::Start("TelescopeDesktop")
$t0 = Get-Date
Open-AsUser "<<menu>>"
$main = Main-Window 300
if (-not $main) { [Watch]::Stop() | ForEach-Object { "WATCH $_" }; throw "the window didn't open within 5 minutes" }
Wait-Until { Find-Control "Settings" "Button" 1 } 60 | Out-Null
"READY $(((Get-Date) - $t0).TotalSeconds)"
<<more>>
[Watch]::Stop() | ForEach-Object { "WATCH $_" }
""").replace('"<<menu>>"', f'"{MENU}"').replace("<<more>>", more), timeout=420)
    a.last_start_rows = watch_log(out)
    a.last_start_out = out
    return float(re.search(r"READY ([\d.,]+)", out).group(1).replace(",", "."))


@check
def clean_install(a):
    setup = upload(a, a.build / "TelescopeSetup.exe")
    clean(a)
    since = win_now(a)
    out = a.win.desktop(ps(r"""
Open-AsUser <<setup>>
# Its buttons are drawn by setup itself, so they show up as panes that need a real click
$deadline = (Get-Date).AddMinutes(5)
while ($true) {
    if ((Get-Date) -gt $deadline) { throw "setup didn't get to Finish: $((Get-Texts 'TelescopeSetup') -join ' / ')" }
    if (Find-Control "Finish" "" 1 "TelescopeSetup") { break }
    $next = Find-Control "Install" "" 1 "TelescopeSetup"
    if (-not $next) { $next = Find-Control "Next" "" 1 "TelescopeSetup" }
    if ($next) { Press $next; Start-Sleep 1 }
}
# Untick "Open Telescope" (the first row of a check list that doesn't name its rows): it's opened from the Start menu next
$list = Get-AppWindows "TelescopeSetup" | ForEach-Object { $_.FindAll($Scope::Descendants, [Windows.Automation.Condition]::TrueCondition) } |
    Where-Object { $_.Current.ClassName -eq "TNewCheckListBox" } | Select-Object -First 1
if ($list) { $r = $list.Current.BoundingRectangle; [Watch]::Click([int]$r.X + 10, [int]$r.Y + 10) }
Start-Sleep 1
Press-Control "Finish" "" 5 "TelescopeSetup"
Start-Sleep 3
if (Get-Process TelescopeDesktop -ErrorAction SilentlyContinue) { throw "unticking Open Telescope didn't stop it opening" }
"OK"
""", setup=setup), timeout=420)
    need("OK" in out, out[-300:])
    version = installed(a)
    need(version == release(a.build), f"installed is {version or 'nothing'}")
    found = a.win.admin(f'Test-Path "{APP}\\TelescopeDesktop.exe"; Test-Path "{MENU}"')
    need(found.split() == ["True", "True"], "the app or its Start menu entry is missing")
    asked = elevations(a, since)
    need(not asked, f"setup asked for admin: {asked}")
    return version


@check
def first_start(a):
    need_installed(a, a.build, "clean_install")
    # Add phone and Settings during the first minute, while it may still be finishing loading
    seconds = first_start_time(a, more=r"""
Press-Control "Add phone" "Button"; Need-Control "Add phone" "Window" 20 | Out-Null; Start-Sleep 2; Close-Window "Add phone"
Press-Control "Settings" "Button"; Need-Control "Settings" "Window" 20 | Out-Null; Start-Sleep 2; Close-Window "Settings"
Wait-Until { ((Get-Date) - $t0).TotalSeconds -gt 75 } 90 | Out-Null
""")
    rows = a.last_start_rows
    shown = [r for r in rows if not r["gone"]]
    need(shown, "no window showed up at all")
    splash = shown[0]
    need(splash["title"] != "Telescope" or seconds < 10, f"no splash: the first window was the main one, after {seconds:.0f} s")
    need(splash["t"] < 15, f"the splash took {splash['t']:.0f} s to show up")
    hung = hung_seconds(rows)
    need(hung < 2, f"the window stopped responding for {hung:.1f} s")
    # Most of a first start is Defender scanning files it hasn't seen, so the time only means something on a fresh VM
    need(seconds < 90, f"the window took {seconds:.0f} s")
    return f"splash after {splash['t']:.1f} s, window ready after {seconds:.0f} s"


@check
def icon(a):
    out = a.win.admin(ps(r"""
Add-Type -AssemblyName System.Drawing
$exe = ([Drawing.Icon]::ExtractAssociatedIcon("<<app>>\TelescopeDesktop.exe")).ToBitmap()
Add-Type -AssemblyName PresentationCore  # System.Drawing can't read the icon's PNG frames
$frames = [Windows.Media.Imaging.BitmapDecoder]::Create([Uri]<<ico>>, 'None', 'OnLoad').Frames
$frame = $frames | Where-Object PixelWidth -eq $exe.Width | Select-Object -First 1
$encoder = New-Object Windows.Media.Imaging.PngBitmapEncoder
$encoder.Frames.Add([Windows.Media.Imaging.BitmapFrame]::Create($frame))
$m = New-Object IO.MemoryStream; $encoder.Save($m)
$want = New-Object Drawing.Bitmap($m)
$diff = 0
for ($x = 0; $x -lt $exe.Width; $x++) { for ($y = 0; $y -lt $exe.Height; $y++) {
    $p = $exe.GetPixel($x, $y); $q = $want.GetPixel($x, $y)
    $diff += [Math]::Abs($p.R - $q.R) + [Math]::Abs($p.G - $q.G) + [Math]::Abs($p.B - $q.B) + [Math]::Abs($p.A - $q.A)
} }
[Math]::Round($diff / ($exe.Width * $exe.Height * 4), 1)
""", ico=upload(a, REPO / "desktop" / "resources" / "telescope.ico")).replace('"<<app>>', f'"{APP}'))
    need(float(out.strip().replace(",", ".")) < 8, f"TelescopeDesktop.exe's icon isn't Telescope's (off by {out.strip()} per channel)")


@check
def driver_install(a):
    need_installed(a, a.build, "clean_install")
    since = win_now(a)
    out = a.win.desktop(r"""
if (-not (Main-Window 120)) { Open-AsUser "$env:LOCALAPPDATA\Programs\Telescope\TelescopeDesktop.exe"; if (-not (Main-Window 120)) { throw "Telescope didn't open" } }
Press-Control "Install driver" "Button" 30
if (-not (Wait-Until { (Get-Texts) -match "^Installed" } 120 1)) { throw "the checklist never said Installed: $((Get-Texts) -join ' / ')" }
"OK"
""", timeout=300)
    need("OK" in out, out[-300:])
    asked = elevations(a, since)
    need(asked == ["powershell.exe from TelescopeDesktop.exe"], f"expected one admin prompt for Windows PowerShell, got {asked or 'none'}")
    cameras = a.win.admin(CAMERAS).splitlines()
    if windows_11(a):
        need(cameras == ["mf Telescope"], f"expected just the Windows 11 camera, apps see {cameras or 'no cameras'}")
        folder = "VirtualCamera"
    else:
        need("64 Telescope" in cameras and "32 Telescope" in cameras, f"DirectShow lists {cameras or 'no cameras'}")
        folder = "UnityCapture"
    owner = a.win.admin(rf'(Get-Acl "$env:ProgramFiles\Telescope\{folder}").Owner')
    need(owner.endswith("Administrators"), f"Program Files\\Telescope\\{folder} is owned by {owner or 'nobody (missing)'}")
    return "every app sees it as Telescope" if windows_11(a) else "OBS, Zoom and browsers see it as Telescope"


def open_dialog_watched(a, opener: str, title: str) -> list:
    out = a.win.desktop(ps(r"""
[Watch]::Start("TelescopeDesktop")
Start-Sleep -Milliseconds 300
<<opener>>
Need-Control <<title>> "Window" 20 | Out-Null
Start-Sleep 2
[Watch]::Stop() | ForEach-Object { "WATCH $_" }
Close-Window <<title>>
""", title=title).replace("<<opener>>", opener), timeout=120)
    return watch_log(out)


@check
def dialogs_centred(a):
    need_installed(a, a.build, "clean_install")
    a.win.desktop(r'if (-not (Main-Window 1)) { Open-AsUser "$env:LOCALAPPDATA\Programs\Telescope\TelescopeDesktop.exe"; Main-Window 120 | Out-Null }')
    dialogs = {
        "Settings": 'Press-Control "Settings" "Button"',
        "Add phone": 'Press-Control "Add phone" "Button"',
        "Your phones": 'Press-Control "Your phones" "Button"',
        "Keyboard shortcut": 'Press-Control "Settings" "Button"; Press-Control "Keyboard shortcuts" "ListItem"; '
                             'Press-Control "Add" "Button"',
    }
    problems, flashes = [], 0
    for title, opener in dialogs.items():
        for _ in range(3):
            rows = open_dialog_watched(a, opener, title)
            if title == "Keyboard shortcut":
                a.win.desktop('Close-Window "Settings"')
            mine = [r for r in rows if not r["gone"] and r["title"] == title]
            main = next((r for r in rows if not r["gone"] and r["title"] == "Telescope"), None)
            strays = [r for r in rows if not r["gone"] and r["title"] not in (title, "Telescope", "Settings")]
            if not mine:
                problems.append(f"{title} never showed")
                continue
            first, last = mine[0]["rect"], mine[-1]["rect"]
            if first != last:
                problems.append(f"{title} moved from {first} to {last}")
            if strays:
                flashes += 1
                problems.append(f"{title} flashed {[(r['title'], r['rect']) for r in strays][:2]}")
            if main:
                (ml, mt, mr, mb), (l, t, r, b) = main["rect"], last
                if abs((l + r) - (ml + mr)) > 80 or (abs((t + b) - (mt + mb)) > 80 and t > 0):
                    problems.append(f"{title} at {last} isn't centred on the window at {main['rect']}")
    need(not problems, "; ".join(problems))
    return "4 dialogs opened 3 times each"


def open_app(a):
    a.win.desktop(r'if (-not (Main-Window 1)) { Open-AsUser "$env:LOCALAPPDATA\Programs\Telescope\TelescopeDesktop.exe"; '
                  r'if (-not (Main-Window 180)) { throw "Telescope didn''t open" } }')


def driver_row(a) -> str:
    """Settings, Virtual camera & phone: what the Driver row says, read fresh."""
    out = a.win.desktop(r"""
if (Find-Control "Settings" "Window" 1) { Close-Window "Settings"; Start-Sleep 1 }
Press-Control "Settings" "Button"
Press-Control "Virtual camera & phone" "ListItem"
Start-Sleep 3
$texts = Get-Texts
foreach ($t in "Ready", "Reinstall for a security fix", "Some apps can't see it", "Update ready", "Not installed") { if ($texts -contains $t) { "ROW $t" } }
$texts | Where-Object { $_ -like "Apps list it as*" } | ForEach-Object { "ROW $_" }
""")
    rows = [line[4:] for line in out.splitlines() if line.startswith("ROW ")]
    return rows[0] if rows else ""


def register_driver(a, folder: str, name: str):
    """Register UnityCapture from folder under name, the way an older Telescope did."""
    out = a.win.admin(ps(r"""
foreach ($view in 'HKLM:\SOFTWARE\Classes', 'HKLM:\SOFTWARE\WOW6432Node\Classes') {
    Get-ChildItem "$view\CLSID" -ErrorAction SilentlyContinue | Where-Object { $_.PSChildName -like '{5C2CD55C-92AD-4999-8666-912BD3E7*' } | Remove-Item -Recurse -Force
    Get-ChildItem "$view\CLSID\{860BB310-5D01-11d0-BD3B-00A0C911CE86}\Instance" -ErrorAction SilentlyContinue |
        Where-Object { $_.PSChildName -like '{5C2CD55C-92AD-4999-8666-912BD3E7*' } | Remove-Item -Recurse -Force
}
foreach ($pair in @(@("$env:windir\System32\regsvr32.exe", 'UnityCaptureFilter64.dll'), @("$env:windir\SysWOW64\regsvr32.exe", 'UnityCaptureFilter32.dll'))) {
    $p = Start-Process $pair[0] -ArgumentList '/s', ('"/i:UnityCaptureName=' + <<name>> + '"'), ('"' + (Join-Path "<<folder>>" $pair[1]) + '"') -Wait -PassThru
    if ($p.ExitCode -ne 0) { "REGSVR32 $($pair[1]) $($p.ExitCode)" }
}""", name=name).replace("<<folder>>", folder))
    need("REGSVR32" not in out, f"couldn't register UnityCapture from {folder}: {out}")


def remove_mf_camera(a):
    """Take the Windows 11 camera away again, to see what an older Telescope's UnityCapture alone looks like."""
    a.win.admin(r"""
$vcam = "$env:ProgramFiles\Telescope\VirtualCamera\TelescopeCamera.dll"
if (Test-Path $vcam) { Start-Process "$env:windir\System32\regsvr32.exe" '/s', '/u', "`"$vcam`"" -Wait }
""")


def driver_older_versions_11(a):
    """Windows 11: an older Telescope's UnityCapture, however it was registered, gets replaced by the Windows 11 camera."""
    notes = []
    for folder, name, note in ((r"$env:LOCALAPPDATA\Programs\Telescope\unitycapture", "Telescope", "from the app folder"),
                               (r"$env:ProgramFiles\Telescope\UnityCapture", "Unity Video Capture", "under its own name")):
        remove_mf_camera(a)
        if folder.startswith("$env:ProgramFiles"):  # where Telescope 3.1.2 and later put it
            copied = a.win.admin(r"""
$dir = "$env:ProgramFiles\Telescope\UnityCapture"; New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item "$env:LOCALAPPDATA\Programs\Telescope\unitycapture\UnityCaptureFilter*.dll" $dir -Force
Get-ChildItem $dir -Filter "UnityCaptureFilter*.dll" | ForEach-Object { "$($_.Name) $($_.Length)" }""")
            need(len(copied.split()) == 4, f"couldn't copy UnityCapture into Program Files: {copied or 'nothing there'}")
        register_driver(a, folder, name)
        row = driver_row(a)
        need(row == "Some apps can't see it",
             f"with UnityCapture registered {note} the row says {row!r} and apps see {a.win.admin(CAMERAS).splitlines()}")
        since = win_now(a)
        a.win.desktop('Press-Control "Driver" "Button"; Wait-Until { (Get-Texts) -contains "Ready" } 120 1 | Out-Null')
        asked = elevations(a, since)
        need(asked == ["powershell.exe from TelescopeDesktop.exe"], f"Update asked for admin {len(asked)} times: {asked}")
        row = driver_row(a)
        cameras = a.win.admin(CAMERAS).splitlines()
        need(row == "Ready" and cameras == ["mf Telescope"],
             f"after Update from UnityCapture {note} the row says {row!r} and apps see {cameras}")
        notes.append(note)
    a.win.desktop('Close-Window "Settings"')
    return "Update replaces UnityCapture " + " and ".join(notes) + " with the Windows 11 camera"


@check
def driver_older_versions(a):
    need_installed(a, a.build, "clean_install")
    open_app(a)
    if windows_11(a):
        return driver_older_versions_11(a)
    notes = []
    # 3.1.1 and older registered the driver straight from the app folder, which the user's programs can write to
    register_driver(a, r"$env:LOCALAPPDATA\Programs\Telescope\unitycapture", "Telescope")
    row = driver_row(a)
    need(row == "Reinstall for a security fix", f"with the driver in the app folder the row says {row!r}")
    since = win_now(a)
    a.win.desktop('Press-Control "Driver" "Button"; Wait-Until { (Get-Texts) -contains "Ready" } 120 1 | Out-Null')
    asked = elevations(a, since)
    need(asked == ["powershell.exe from TelescopeDesktop.exe"], f"Reinstall asked for admin {len(asked)} times: {asked}")
    row = driver_row(a)
    need(row == "Ready", f"after Reinstall the row says {row!r}")
    notes.append("security fix moves it to Program Files")
    # Older still: registered under UnityCapture's own name
    register_driver(a, r"$env:ProgramFiles\Telescope\UnityCapture", "Unity Video Capture")
    row = driver_row(a)
    need(row.startswith("Apps list it as"), f"with the old name the row says {row!r}")
    a.win.desktop('Press-Control "Driver" "Button"; Wait-Until { (Get-Texts) -contains "Ready" } 120 1 | Out-Null')
    row = driver_row(a)
    cameras = a.win.admin(CAMERAS).splitlines()
    need(row == "Ready" and "64 Telescope" in cameras, f"after Rename the row says {row!r} and DirectShow lists {cameras}")
    notes.append("Rename gives it Telescope's name")
    a.win.desktop('Close-Window "Settings"')
    return ", ".join(notes)


def plugin_zip(a) -> str:
    """The black and white example as a plugin zip with the folder itself inside, on the Windows machine."""
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "plugins" / "black_and_white.zip"
        path.parent.mkdir()
        with zipfile.ZipFile(path, "w") as z:
            for f in sorted(PLUGIN.rglob("*")):
                if f.is_file() and "__pycache__" not in f.parts:
                    z.write(f, f"black_and_white/{f.relative_to(PLUGIN).as_posix()}")
        return upload(a, path)


@check
def plugin_install(a):
    need_installed(a, a.build, "clean_install")
    open_app(a)
    zip_path = plugin_zip(a)
    info = json.loads((PLUGIN / "plugin.json").read_text())
    a.win.admin(r'Remove-Item -Recurse -Force "$env:APPDATA\Telescope\plugins\black_and_white" -ErrorAction SilentlyContinue')
    out = a.win.desktop(ps(r"""
if (Find-Control "Settings" "Window" 1) { Close-Window "Settings"; Start-Sleep 1 }
Press-Control "Settings" "Button"
Press-Control "Plugins" "ListItem"
Press-Control "Install" "Button"
Need-Control "Install a plugin" "Window" 20 | Out-Null
if (-not [Watch]::OpenInFileDialog("Install a plugin", <<zip>>)) { throw "couldn't fill in the file dialog" }
Press-Control "OK" "Button" 20
Press-Control "<<title>> <<version>>" "CheckBox" 10
Press-Control "Switch on" "Button" 10
Wait-Restart { Press-Control "Restart now" "Button" 20 }
if (-not (Find-Control "<<title>>" "CheckBox" 30)) { throw "no <<title>> card after the restart" }
"OK"
""", zip=zip_path).replace("<<title>>", info["title"]).replace("<<version>>", info["version"]), timeout=400)
    need("OK" in out, out[-300:])
    return "Install picks the zip, switching on and Restart now shows its card"


@check
def plugin_drag(a):
    need_installed(a, a.build, "clean_install")
    open_app(a)
    zip_path = plugin_zip(a)
    info = json.loads((PLUGIN / "plugin.json").read_text())
    out = a.win.desktop(ps(r"""
if (Find-Control "Settings" "Window" 1) { Close-Window "Settings"; Start-Sleep 1 }
Press-Control "Settings" "Button"
Press-Control "Plugins" "ListItem"
$remove = Find-Control "Remove <<title>>" "Button" 5
if ($remove) { Press $remove; Press-Control "Remove" "Button" 10; Start-Sleep 2 }
if (Find-Control "<<title>> <<version>>" "CheckBox" 2) { throw "Remove left it in the list" }
# Explorer showing the zip, in the top-left corner, and the zip dragged onto the plugins page
Open-AsUser (Split-Path <<zip>>)
$item = Need-Control (Split-Path -Leaf <<zip>>) "ListItem" 30 "explorer"
$win = [Watch]::Windows("explorer") | Where-Object { $_.Split('|')[2] -like "plugins*" } | Select-Object -First 1
if ($win) { [Watch]::Place([long]$win.Split('|')[0], 0, 0, 520, 420); Start-Sleep 1 }
$item = Need-Control (Split-Path -Leaf <<zip>>) "ListItem" 10 "explorer"
$from = $item.Current.BoundingRectangle
$page = (Need-Control "Settings" "Window").Current.BoundingRectangle
[Watch]::Drag([int]($from.X + 20), [int]($from.Y + $from.Height / 2), [int]($page.Right - 200), [int]($page.Bottom - 120))
Press-Control "OK" "Button" 20
$ok = [bool](Find-Control "<<title>> <<version>>" "CheckBox" 10)
if ($win) { (New-Object -ComObject Shell.Application).Windows() | Where-Object { $_.LocationName -eq "plugins" } | ForEach-Object { $_.Quit() } }
"LISTED $ok"
""", zip=zip_path).replace("<<title>>", info["title"]).replace("<<version>>", info["version"]), timeout=300)
    need(line(out, "LISTED") == "True", f"dragging the zip in from Explorer didn't add it: {out[-200:]}")
    return "Remove takes it out, dragging the zip in from Explorer adds it back"


@check
def get_adb(a):
    need_installed(a, a.build, "clean_install")
    if not a.agree_adb_licence:
        raise Skip("Get adb needs Google's SDK licence accepted: pass --agree-adb-licence")
    open_app(a)
    out = a.win.desktop(r"""
if (Find-Control "Settings" "Window" 1) { Close-Window "Settings"; Start-Sleep 1 }
Press-Control "Settings" "Button"
Press-Control "Virtual camera & phone" "ListItem"
Press-Control "Get adb" "Button"
Press-Control "Agree and download" "Button" 20
$adb = "$env:LOCALAPPDATA\Telescope\platform-tools\adb.exe"
if (-not (Wait-Until { Test-Path $adb } 180 2)) { throw "adb.exe didn't show up" }
Start-Sleep 5
Close-Window "Settings"
Close-Window "Telescope"  # with nothing streaming, closing the window quits
if (-not (Wait-Until { -not (Get-Process TelescopeDesktop -ErrorAction SilentlyContinue) } 30 1)) { throw "Telescope didn't quit" }
Start-Sleep 3
"ADB $(@(Get-Process adb -ErrorAction SilentlyContinue).Count)"
""", timeout=400)
    left = re.search(r"ADB (\d+)", out)
    need(left, out[-300:])
    need(left.group(1) == "0", "adb.exe is still running after quitting Telescope")
    return "downloads adb, nothing left running after quitting"


@check
def uninstall(a):
    need_installed(a, a.build, "clean_install")
    a.win.admin("Get-Process TelescopeDesktop* -ErrorAction SilentlyContinue | Stop-Process -Force")
    since = win_now(a)
    # Settings, Apps runs the uninstall entry's command as the user
    out = a.win.desktop(ps(r"""
$cmd = (Get-ItemProperty <<key>>).UninstallString.Trim('"')
Open-AsUser $cmd
Press-Control "Yes" "" 30 ""  # are you sure
$asked = @()
$deadline = (Get-Date).AddMinutes(3)
while ((Get-Date) -lt $deadline) {
    $texts = Get-Texts ""
    if ($texts -match "Also remove the Telescope virtual camera") { $asked += "camera"; Press-Control "Yes" "" 5 ""; Start-Sleep 2; continue }
    if ($texts -match "Also delete your Telescope settings") { $asked += "settings"; Press-Control "No" "" 5 ""; Start-Sleep 2; continue }
    if ($texts -match "successfully removed") { Press-Control "OK" "" 5 ""; break }
    Start-Sleep 1
}
"ASKED $($asked -join ',')"
""", key=UNINSTALL_KEY), timeout=300)
    asked_about = re.search(r"ASKED (.*)", out)
    need(asked_about and asked_about.group(1).strip() == "camera,settings",
         f"expected to be asked about the camera, then the settings: {out[-200:]}")
    prompts = elevations(a, since)
    need(len(prompts) == 1 and prompts[0].startswith("powershell.exe from"),
         f"expected one admin prompt for Windows PowerShell, got {prompts or 'none'}")
    left = a.win.admin(rf"""
Start-Sleep 3
if (Test-Path "{APP}") {{ "app folder" }}
# A DLL Frame Server still has loaded goes at the next restart, which the uninstaller queues
$pending = @((Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager').PendingFileRenameOperations)
$stays = @(Get-ChildItem "$env:ProgramFiles\Telescope" -Recurse -File -Force -ErrorAction SilentlyContinue | Where-Object {{ $pending -notcontains ('\??\' + $_.FullName) }})
if ((Test-Path "$env:ProgramFiles\Telescope") -and ($stays -or $pending -notcontains "\??\$env:ProgramFiles\Telescope")) {{ "Program Files\Telescope" }}
if (Test-Path "{MENU}") {{ "Start menu entry" }}
if (-not (Test-Path "$env:APPDATA\Telescope")) {{ "settings were deleted" }}
""" + CAMERAS.replace('"$bits " +', '"camera " +'))
    need(not left.strip(), f"left behind or wrong: {', '.join(left.split(chr(10)))}")
    return "asks about the camera and settings, one admin prompt, keeps settings"


@check
def update_installed(a):
    if not a.previous:
        raise Skip("no --previous release to update from")
    setup = upload(a, a.previous / "TelescopeSetup.exe")
    clean(a)
    a.win.admin(ps("Start-Process <<s>> '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait", s=setup))
    need_installed(a, a.previous, "the previous release's setup")
    open_app(a)
    since = win_now(a)
    channel = "Nightly" if manifest(a.build)["channel"] == "nightly" else "Stable"
    out = a.win.desktop(r"""
if (-not (Main-Window 120)) { throw "Telescope isn't running" }
if (-not (Find-Control "Settings" "Window" 1)) { Press-Control "Settings" "Button" }
Press-Control "Updates & About" "ListItem"
Choose "Channel" "<<channel>>"
""".replace("<<channel>>", channel) + r"""
Press-Control "Check now" "Button"
Wait-Restart { Press-Control "Update and restart" "Button" 120 }
"OK"
""", timeout=700)
    need("OK" in out, out[-300:])
    after = installed(a)
    need(after and after != release(a.previous), f"still {after or 'nothing'} installed")
    need(not elevations(a, since), "the update asked for admin")
    return f"{release(a.previous)} to {after} ({channel} channel)"


UNZIP_AND_OPEN = r"""
$dir = $ExecutionContext.InvokeCommand.ExpandString(<<dir>>)
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
Expand-Archive <<zip>> $dir
Open-AsUser "$dir\TelescopeDesktop.exe"
if (-not (Main-Window 300)) { throw "the unzipped copy didn't open" }
Press-Control "Settings" "Button"
Press-Control "Monochrome" "CheckBox"  # a setting to look for after the update
Press-Control "Updates & About" "ListItem"
Choose "Channel" <<channel>>
Press-Control "Check now" "Button"
if (-not (Wait-Until { (Get-Texts) -match "is available" } 120 2)) { throw "no update offered: $((Get-Texts) -match 'version|Telescope ' -join ' / ')" }
Start-Sleep 1
$texts = Get-Texts
"MOVE $([bool]($texts -match 'also installs it for your user'))"
"""


def unzipped_previous(a, folder: str, before_update: str = "") -> str:
    """Unzip the previous release to folder, open it, pick a setting and the channel and update; what it printed."""
    zip_path = upload(a, a.previous / "Telescope-windows-portable.zip")
    channel = "Nightly" if manifest(a.build)["channel"] == "nightly" else "Stable"
    return a.win.desktop(ps(UNZIP_AND_OPEN, dir=folder, zip=zip_path, channel=channel) + before_update + r"""
Wait-Restart { Press-Control "Update and restart" "Button" 60 }
$p = Get-Process TelescopeDesktop | Select-Object -First 1
"RUNNING $($p.Path)"
Press-Control "Settings" "Button"
Press-Control "General" "ListItem"
"MONOCHROME $(Is-On (Need-Control 'Monochrome' 'CheckBox'))"
Close-Window "Settings"
""", timeout=900)


def line(out: str, key: str) -> str:
    m = re.search(rf"^{key} (.*)$", out, re.M)
    return m.group(1).strip() if m else ""


@check
def unzipped_update_installs(a):
    if not a.previous:
        raise Skip("no --previous release to update from")
    clean(a)
    since = win_now(a)
    out = unzipped_previous(a, "$env:USERPROFILE\\Downloads\\Telescope-old")
    need(line(out, "MOVE") == "True", "Updates didn't say updating installs it for your user")
    running = line(out, "RUNNING")
    need(running.lower().endswith(r"\appdata\local\programs\telescope\telescopedesktop.exe"),
         f"it came back running from {running or 'nowhere'}, not the installed copy")
    need(line(out, "MONOCHROME") == "True", "the Monochrome setting didn't survive the update")
    after = installed(a)
    need(after and after != release(a.previous), f"installed is {after or 'nothing'}")
    gone = a.win.admin(rf'Test-Path "$env:USERPROFILE\Downloads\Telescope-old\TelescopeDesktop.exe"; Test-Path "{MENU}"')
    need(gone.split() == ["False", "True"], "the unzipped folder is still there or there's no Start menu entry")
    asked = elevations(a, since)
    need(not asked, f"it asked for admin: {asked}")
    return f"{release(a.previous)} unzipped became {after} installed, settings kept"


@check
def unzipped_keep_in_place(a):
    if not a.previous:
        raise Skip("no --previous release to update from")
    clean(a)
    out = unzipped_previous(a, "$env:USERPROFILE\\Downloads\\Telescope-old",
                            before_update='\nPress-Control "Keep this copy where it is" "CheckBox"\nStart-Sleep 1\n')
    running = line(out, "RUNNING")
    need(running.lower().endswith(r"\downloads\telescope-old\telescopedesktop.exe"), f"it came back running from {running}")
    need(line(out, "MONOCHROME") == "True", "the Monochrome setting didn't survive the update")
    left = a.win.admin(rf'Test-Path "{APP}"; Test-Path "{MENU}"; (Get-ChildItem "$env:USERPROFILE\Downloads\Telescope-old" -Directory -Filter lib-*).Name')
    need(left.split()[:2] == ["False", "False"], "it installed itself anyway")
    builds = [x for x in left.split()[2:] if x.startswith("lib-")]
    need(builds and builds != [f"lib-{manifest(a.previous)['build']}"], f"the folder still has {builds}")
    return f"updated in place to {builds[0]}"


@check
def carried_around(a):
    if not a.previous:
        raise Skip("no --previous release to look at")
    clean(a)
    zip_path = upload(a, a.previous / "Telescope-windows-portable.zip")
    # A copy on a network drive goes by the same rule as one on a USB stick: it's portable on purpose
    a.win.admin(r"""
$dir = 'C:\TelescopeTest\share'
New-Item -ItemType Directory -Force $dir | Out-Null
if (-not (Get-SmbShare TelescopeTest -ErrorAction SilentlyContinue)) { New-SmbShare TelescopeTest $dir -FullAccess Everyone | Out-Null }
""")
    channel = "Nightly" if manifest(a.build)["channel"] == "nightly" else "Stable"
    out = a.win.desktop(ps(r"""
cmd /c "net use T: /delete /y >nul 2>&1"
cmd /c "net use T: \\localhost\TelescopeTest >nul"
if (-not (Test-Path T:\)) { throw "couldn't map the share to T:" }
""") + ps(UNZIP_AND_OPEN, dir=r"T:\Telescope", zip=zip_path, channel=channel) + r"""
Close-Window "Settings"
Close-Window "Telescope"
Start-Sleep 3
cmd /c "net use T: /delete /y >nul 2>&1"
""", timeout=600)
    need(line(out, "MOVE") == "False", "a copy on a network drive offered to install itself for the user")
    return "a copy on a network drive updates where it is"


@check
def update_cut_short(a):
    if not a.previous:
        raise Skip("no --previous release to update from")
    clean(a)
    zip_path = upload(a, a.previous / "Telescope-windows-portable.zip")
    channel = "Nightly" if manifest(a.build)["channel"] == "nightly" else "Stable"
    out = a.win.desktop(ps(UNZIP_AND_OPEN, dir="$env:USERPROFILE\\Downloads\\Telescope-old", zip=zip_path, channel=channel) + r"""
Press-Control "Keep this copy where it is" "CheckBox"
Start-Sleep 1
Press-Control "Update and restart" "Button" 60
# Pull the plug the moment the swap starts
$journal = "$dir\.update.json"
if (-not (Wait-Until { (Test-Path $journal) -and ((Get-Content $journal -Raw -ErrorAction SilentlyContinue) -match '"swapping"') } 300 0.02)) { throw "the swap never started" }
Get-Process TelescopeDesktop -ErrorAction SilentlyContinue | Stop-Process -Force
"KILLED"
Start-Sleep 2
Open-AsUser "$dir\TelescopeDesktop.exe"
if (-not (Main-Window 300)) { throw "it didn't open again after being cut short" }
Start-Sleep 10
"LIBS $((Get-ChildItem $dir -Directory -Filter lib-*).Name -join ',')"
"JOURNAL $(Test-Path $journal)"
Close-Window "Telescope"
""", timeout=900)
    need("KILLED" in out, out[-300:])
    libs = line(out, "LIBS").split(",")
    need(len(libs) == 1, f"it left several library folders: {libs}")
    need(line(out, "JOURNAL") == "False", "the update journal is still there")
    old = f"lib-{manifest(a.previous)['build']}"
    return "finished the update on the next start" if libs[0] != old else "came back on the old version"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("build", type=Path, nargs="?", help="folder from fetch_nightly.sh with the release to check")
    parser.add_argument("--ssh", help='how to reach the Windows machine, as ssh arguments with the destination last ("-p 2222 user@host")')
    parser.add_argument("--previous", type=Path, help="an older release's folder, for the update checks")
    parser.add_argument("--agree-adb-licence", action="store_true",
                        help="accept Google's SDK licence in Get adb, so adb gets downloaded and checked")
    parser.add_argument("--only", nargs="+", metavar="NAME", help="run just these checks (they may lean on earlier ones)")
    parser.add_argument("--list", action="store_true", help="list the checks and stop")
    a = parser.parse_args()
    if a.list:
        print("\n".join(fn.__name__ for fn in CHECKS))
        return 0
    if not a.build or not a.ssh:
        parser.error("the release folder and --ssh are needed")
    a.win = Windows(a.ssh)
    a.build_number = None
    a.win.put((HERE / "windows" / "ui.ps1").read_bytes(), rf"{WORK}\ui.ps1")
    # UAC prompts sit on the secure desktop where nothing can press them: elevate without asking for the run and
    # count elevations from the security log instead
    before = a.win.admin(ps(r"""
"$((Get-ItemProperty <<key>>).ConsentPromptBehaviorAdmin)"
(auditpol /get /subcategory:<<audit>> /r | ConvertFrom-Csv).'Inclusion Setting'
Set-ItemProperty <<key>> ConsentPromptBehaviorAdmin 0
auditpol /set /subcategory:<<audit>> /success:enable | Out-Null
""", key=UAC_KEY, audit=PROCESS_AUDIT)).splitlines()

    results = []
    try:
        for fn in CHECKS:
            if a.only and fn.__name__ not in a.only:
                continue
            try:
                note = fn(a)
                results.append(("PASS", fn.__name__, note or ""))
            except Skip as e:
                results.append(("SKIP", fn.__name__, str(e)))
            except Fail as e:
                results.append(("FAIL", fn.__name__, str(e)))
            except Exception as e:  # a broken check shouldn't stop the rest
                results.append(("FAIL", fn.__name__, f"{type(e).__name__}: {e}"))
            print(f"{results[-1][0]:<5} {fn.__name__:<26} {results[-1][2]}", flush=True)
    finally:
        audit = "enable" if "Success" in before[1] else "disable"
        a.win.admin(ps(f"""
Set-ItemProperty <<key>> ConsentPromptBehaviorAdmin {int(before[0] or 5)}
auditpol /set /subcategory:<<audit>> /success:{audit} | Out-Null
""", key=UAC_KEY, audit=PROCESS_AUDIT))
    failed = sum(r[0] == "FAIL" for r in results)
    print(f"\n{len(results) - failed} of {len(results)} passed or skipped, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
