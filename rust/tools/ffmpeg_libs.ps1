# Makes FFMPEG_DIR's .lib files again from its .def files: dlltool's can't be delay-loaded through by link.exe, and ours come from mingw.
# Needs MSVC's lib.exe, which it finds on its own. The folder is $env:FFMPEG_DIR.
$ErrorActionPreference = 'Stop'
if (-not $env:FFMPEG_DIR) { throw 'Set FFMPEG_DIR to a shared FFmpeg build' }
$tools = (Get-ChildItem "${env:ProgramFiles(x86)}\Microsoft Visual Studio\*\*\VC\Tools\MSVC\*\bin\Hostx64\x64\lib.exe", "$env:ProgramFiles\Microsoft Visual Studio\*\*\VC\Tools\MSVC\*\bin\Hostx64\x64\lib.exe" -ErrorAction SilentlyContinue | Select-Object -Last 1).DirectoryName
if (-not $tools) { throw "MSVC's lib.exe wasn't found" }
Push-Location "$env:FFMPEG_DIR\lib"
foreach ($def in Get-ChildItem *.def) {
    & "$tools\lib.exe" /nologo /machine:x64 "/def:$($def.Name)" "/out:$($def.BaseName -replace '-\d+$', '').lib" | Out-Null
    if ($LASTEXITCODE) { throw "lib.exe failed on $($def.Name)" }
}
Pop-Location
