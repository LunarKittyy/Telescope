# Builds telescope-rs.exe with H.264 and puts it with its FFmpeg DLLs in rust/target/dist-windows (or -Out).
# Needs MSVC, LLVM (bindgen), CMake, NASM, and FFMPEG_DIR pointing at a shared FFmpeg 8.x with include, lib and bin.
# Run it from a shell where those are on PATH.
param([string]$Out = "$PSScriptRoot\..\target\dist-windows")

$ErrorActionPreference = 'Stop'
if (-not $env:FFMPEG_DIR) { throw 'Set FFMPEG_DIR to a shared FFmpeg build' }
$tools = (Get-ChildItem "${env:ProgramFiles(x86)}\Microsoft Visual Studio\*\*\VC\Tools\MSVC\*\bin\Hostx64\x64\lib.exe" | Select-Object -Last 1).DirectoryName

# Prebuilt FFmpeg's .lib files come from dlltool and link.exe can't delay-load through those, so they're made again from the .def files
Push-Location "$env:FFMPEG_DIR\lib"
foreach ($def in Get-ChildItem *.def) {
    & "$tools\lib.exe" /nologo /machine:x64 "/def:$($def.Name)" "/out:$($def.BaseName -replace '-\d+$', '').lib" | Out-Null
}
Pop-Location

Push-Location "$PSScriptRoot\.."
cargo build --release --locked -p telescope-app --features ffmpeg-windows
if ($LASTEXITCODE) { throw 'cargo build failed' }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
New-Item -ItemType Directory -Force $Out | Out-Null
Copy-Item "$target\release\telescope-rs.exe" $Out
# The window's .slint files and icons are read at run time (telescope_ui_kit::library_path)
Remove-Item -Recurse -Force "$Out\ui-kit" -ErrorAction SilentlyContinue
Copy-Item -Recurse crates\ui-kit\ui "$Out\ui-kit"
foreach ($dll in 'avcodec', 'avutil', 'swscale', 'swresample') {
    Copy-Item "$env:FFMPEG_DIR\bin\$dll-*.dll" $Out
}
Pop-Location
