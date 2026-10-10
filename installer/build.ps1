# Builds TelescopeSetup.exe from an assembled bundle folder (build-windows.yml's bundle/, with Telescope.apk in it
# for a release). Installs Inno Setup first when it isn't there, which needs Chocolatey (GitHub's runners have it).
param(
    [Parameter(Mandatory)] [string] $Bundle,
    [Parameter(Mandatory)] [string] $Out,
    [string] $Build = "0"
)
$ErrorActionPreference = "Stop"

function Find-Iscc {
    $cmd = Get-Command iscc.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($dir in "${env:ProgramFiles(x86)}\Inno Setup 6", "$env:ProgramFiles\Inno Setup 6",
                     "$env:LOCALAPPDATA\Programs\Inno Setup 6") {
        if (Test-Path "$dir\ISCC.exe") { return "$dir\ISCC.exe" }
    }
    return $null
}

$iscc = Find-Iscc
if (-not $iscc) {
    choco install innosetup -y --no-progress | Out-Host
    $iscc = Find-Iscc
    if (-not $iscc) { throw "Inno Setup didn't install" }
}

$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content "$root/VERSION" -Raw).Trim()
$bundlePath = (Resolve-Path $Bundle).Path
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$outPath = (Resolve-Path $Out).Path

& $iscc /Q "/DAppVersion=$version" "/DBuild=$Build" "/DSourceDir=$bundlePath" "/DOutputDir=$outPath" `
    "$PSScriptRoot/Telescope.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with $LASTEXITCODE" }
Write-Host "Built $outPath\TelescopeSetup.exe"
