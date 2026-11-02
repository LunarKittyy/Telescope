# Builds the Windows package folder, the shape of Telescope-windows-portable.zip: TelescopeDesktop.exe with everything next to it.
# Needs MSVC, LLVM (bindgen), CMake, NASM, Python, and FFMPEG_DIR pointing at build_ffmpeg.sh's windows build (or any shared FFmpeg 8.x with include, lib and bin).
# Run it from a shell where those are on PATH. -Channel, -Build and -Commit stamp a release.
param(
    [string]$Out = "$PSScriptRoot\..\target\dist-windows",
    [ValidateSet('stable', 'nightly', 'dev')] [string]$Channel = 'dev',
    [string]$Build = '0',
    [string]$Commit = ''
)

$ErrorActionPreference = 'Stop'
if (-not $env:FFMPEG_DIR) { throw 'Set FFMPEG_DIR to a shared FFmpeg build' }
& "$PSScriptRoot\ffmpeg_libs.ps1"
# Against where it was called from, before the build moves into rust/
$Out = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)

$rust = (Resolve-Path "$PSScriptRoot\..").Path
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$rust\target" }
Push-Location $rust

$env:TELESCOPE_BUILD = $Build
$env:TELESCOPE_CHANNEL = $Channel
$env:TELESCOPE_COMMIT = $Commit
# The C runtime goes into the program, as with the camera's DLL: a clean Windows may not have the VC++ redistributable
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --locked -p telescope-app --features ffmpeg-windows
if ($LASTEXITCODE) { throw 'cargo build failed' }
$exe = "$target\release\telescope-rs.exe"

# The Windows 11 camera, with the wait screen this program draws built into it. It's a window program, so PowerShell has to be told to wait
$work = "$target\virtualcamera"
New-Item -ItemType Directory -Force $work | Out-Null
$wait = "$work\wait_screen.bin"
$p = Start-Process -FilePath $exe -ArgumentList 'camera-wait-screen', "`"$wait`"" -Wait -PassThru -NoNewWindow
if ($p.ExitCode) { throw "camera-wait-screen failed with $($p.ExitCode)" }
cmake -S "$rust\windows\virtualcamera" -B "$work\build" -A x64 "-DTELESCOPE_WAIT_SCREEN=$wait"
if ($LASTEXITCODE) { throw 'cmake failed' }
cmake --build "$work\build" --config Release
if ($LASTEXITCODE) { throw 'The camera DLL failed to build' }

if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
New-Item -ItemType Directory -Force $Out, "$Out\virtualcamera" | Out-Null
$Out = (Resolve-Path $Out).Path
Copy-Item $exe "$Out\TelescopeDesktop.exe"
# The window's .slint files and icons are read at run time (telescope_ui_kit::library_path)
Copy-Item -Recurse crates\ui-kit\ui "$Out\ui-kit"
foreach ($dll in 'avcodec', 'avutil', 'swresample', 'swscale') {
    Copy-Item "$env:FFMPEG_DIR\bin\$dll-*.dll" $Out
}
Copy-Item -Recurse "$rust\windows\unitycapture" "$Out\unitycapture"
Copy-Item "$work\build\Release\TelescopeCamera.dll" "$Out\virtualcamera\"
python tools\fetch_rnnoise.py windows $Out
if ($LASTEXITCODE) { throw 'fetch_rnnoise failed' }
# rnnoise.dll needs it and a clean Windows may not have it. Next to the exe it's also found before a Python build's lib-<build>, which would then stay locked
Copy-Item "$env:SystemRoot\System32\vcruntime140.dll" $Out
python tools\write_notices.py windows $env:FFMPEG_DIR "$Out\THIRD_PARTY_NOTICES.txt"
if ($LASTEXITCODE) { throw 'write_notices failed' }
Pop-Location
Write-Host "Telescope $Channel $Build is in $Out"
