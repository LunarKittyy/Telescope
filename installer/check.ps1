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
New-Item -ItemType Directory -Force "$env:TEMP\Telescope" | Out-Null
Set-Content "$env:TEMP\Telescope\telescope.log" "x"  # the log (diagnostics.py)
New-NetFirewallRule -DisplayName "telescopedesktop.exe" -Direction Inbound -Program "$app\TelescopeDesktop.exe" -Action Allow | Out-Null
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
if (Test-Path "$env:TEMP\Telescope") { $problems += "after uninstall, the log folder is still there" }
# The runner is admin, so the silent uninstall takes the camera the driver check installed, and its firewall rules
if (Test-Path "$env:ProgramFiles\Telescope") { $problems += "after uninstall, Program Files\Telescope (the camera) is still there" }
if (Get-NetFirewallApplicationFilter | Where-Object { $_.Program -eq "$app\TelescopeDesktop.exe" }) {
    $problems += "after uninstall, a firewall rule for Telescope is still there"
}
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

# Telescope still running (in the tray, say) gets closed, so the uninstall can take all of its files
$p = Start-Process -FilePath $Setup -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait -PassThru
$env:QT_QPA_PLATFORM = "offscreen"
$running = Start-Process -FilePath "$app\TelescopeDesktop.exe" -ArgumentList "--minimized" -PassThru
Remove-Item Env:QT_QPA_PLATFORM
Start-Sleep -Seconds 8
if ($running.HasExited) { $problems += "Telescope didn't stay running for the uninstall-while-running check" }
Start-Process -FilePath "$app\unins000.exe" -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait | Out-Null
for ($i = 0; $i -lt 60 -and (Test-Path $app); $i++) { Start-Sleep -Seconds 1 }
if (Test-Path $app) {
    $left = Get-ChildItem $app -Recurse -Force | ForEach-Object { $_.FullName.Substring($app.Length + 1) }
    $problems += "uninstalling while Telescope ran left: $($left -join ', ')"
}
if (-not $running.HasExited) { $problems += "uninstalling left Telescope running"; Stop-Process -Id $running.Id -Force }

# adb still locked when uninstalling (a server that outlived Telescope): the next sign-in deletes it instead
$runOnce = "HKCU:\Software\Microsoft\Windows\CurrentVersion\RunOnce"
$p = Start-Process -FilePath $Setup -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait -PassThru
New-Item -ItemType Directory -Force $adb | Out-Null
Set-Content "$adb\adb.exe" "x"
$lock = [IO.File]::Open("$adb\adb.exe", "Open", "Read", "Read")
Start-Process -FilePath "$app\unins000.exe" -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait | Out-Null
for ($i = 0; $i -lt 60 -and (Test-Path $app); $i++) { Start-Sleep -Seconds 1 }
$lock.Close()
$cleanup = (Get-ItemProperty $runOnce -ErrorAction SilentlyContinue).TelescopeCleanup
if (-not $cleanup) { $problems += "a locked adb left no sign-in clean-up" }
else {
    # Run it the way Windows does at sign-in: the whole line, program first
    Start-Process -FilePath "cmd.exe" -ArgumentList $cleanup.Substring("cmd.exe ".Length) -Wait -WindowStyle Hidden
    if (Test-Path "$env:LOCALAPPDATA\Telescope") { $problems += "the sign-in clean-up didn't delete the adb folder" }
    Remove-ItemProperty $runOnce -Name "TelescopeCleanup"
}

if ($problems) {
    $problems | ForEach-Object { Write-Host "::error::$_" }
    exit 1
}
Write-Host "Installer verified: per-user install, Start menu entry, clean uninstall."
