# Installs TelescopeSetup.exe for this user, looks at what it made, plays a self-update and "Open at sign-in", then
# uninstalls and checks it took everything with it. CI runs it on a fresh runner (build-windows.yml).
param([Parameter(Mandatory)] [string] $Setup)
$ErrorActionPreference = "Stop"

$app = "$env:LOCALAPPDATA\Programs\Telescope"
$menu = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Telescope.lnk"
$runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$problems = @()

$p = Start-Process -FilePath $Setup -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "Setup exited with $($p.ExitCode)" }

foreach ($want in "$app\TelescopeDesktop.exe", "$app\unitycapture", "$app\unins000.exe", $menu) {
    if (-not (Test-Path $want)) { $problems += "after install, missing $want" }
}
if (-not (Get-ChildItem $app -Directory -Filter "lib-*")) { $problems += "after install, no lib-<build> folder" }
if (Test-Path "$env:ProgramFiles\Telescope\TelescopeDesktop.exe") { $problems += "installed into Program Files" }
if (Get-ChildItem $app -Recurse -Filter "adb*.exe") { $problems += "adb is in the install; it's downloaded on request" }

# What the app does to its folder later: an update's next libraries and leftovers, a log shortcut, the Run key
New-Item -ItemType Directory -Force "$app\lib-999999\PyQt6" | Out-Null
Set-Content "$app\lib-999999\PyQt6\x.dll" "x"
New-Item -ItemType Directory -Force "$app\.update-staging" | Out-Null
Set-Content "$app\.update.json" "{}"
Set-Content "$app\TelescopeDesktop.old.exe" "x"
$adb = "$env:LOCALAPPDATA\Telescope\platform-tools"  # adb as Get adb downloads it
New-Item -ItemType Directory -Force $adb | Out-Null
Set-Content "$adb\adb.exe" "x"
Set-ItemProperty $runKey -Name "Telescope" -Value "`"$app\TelescopeDesktop.exe`" --minimized"

$p = Start-Process -FilePath "$app\unins000.exe" -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait -PassThru
if ($p.ExitCode -ne 0) { $problems += "uninstall exited with $($p.ExitCode)" }
# The uninstaller hands over to a copy of itself in %TEMP% and returns, so wait for the folder to go
for ($i = 0; $i -lt 60 -and (Test-Path $app); $i++) { Start-Sleep -Seconds 1 }

if (Test-Path $app) {
    $left = Get-ChildItem $app -Recurse -Force | ForEach-Object { $_.FullName.Substring($app.Length + 1) }
    $problems += "after uninstall, the folder is still there with: $($left -join ', ')"
}
if (Test-Path $menu) { $problems += "after uninstall, the Start menu entry is still there" }
if (Test-Path "$env:LOCALAPPDATA\Telescope") { $problems += "after uninstall, the downloaded adb is still there" }
if ((Get-ItemProperty $runKey -ErrorAction SilentlyContinue).PSObject.Properties.Name -contains "Telescope") {
    $problems += "after uninstall, Telescope still opens at sign-in"
}

# A Run entry for another copy of Telescope (an unzipped one, say) is left alone
$p = Start-Process -FilePath $Setup -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait -PassThru
Set-ItemProperty $runKey -Name "Telescope" -Value "`"C:\Tools\Telescope\TelescopeDesktop.exe`" --minimized"
Start-Process -FilePath "$app\unins000.exe" -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait | Out-Null
for ($i = 0; $i -lt 60 -and (Test-Path $app); $i++) { Start-Sleep -Seconds 1 }
if ((Get-ItemProperty $runKey).Telescope -notlike "*C:\Tools\Telescope*") { $problems += "uninstall removed another copy's sign-in entry" }
Remove-ItemProperty $runKey -Name "Telescope"

if ($problems) {
    $problems | ForEach-Object { Write-Host "::error::$_" }
    exit 1
}
Write-Host "Installer verified: per-user install, Start menu entry, clean uninstall."
