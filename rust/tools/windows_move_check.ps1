# An unzipped copy of the Rust app moving to the installed one, for real: unpacks the bundle into a fake Downloads that already holds the
# user's own things, runs the setup as the app does, starts the installed copy as the app would and checks that only Telescope's files went.
# desktop/scripts/windows_move_check.py is the same check for the Python app. Run it as the user Telescope is for, in a signed-in session.
param(
    [Parameter(Mandatory)] [string] $Setup,
    [Parameter(Mandatory)] [string] $Bundle
)
$ErrorActionPreference = 'Stop'
$problems = @()
$downloads = Join-Path ([IO.Path]::GetTempPath()) "Downloads-$PID"
New-Item -ItemType Directory $downloads | Out-Null
Set-Content "$downloads\holiday.jpg" "not Telescope's"
New-Item -ItemType Directory "$downloads\platform-tools" | Out-Null
Set-Content "$downloads\platform-tools\fastboot.exe" "not Telescope's"
Copy-Item -Recurse -Force "$Bundle\*" $downloads

$p = Start-Process -FilePath $Setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
if ($p.ExitCode) { throw "Setup exited with $($p.ExitCode)" }
$entry = Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' | Where-Object { $_.PSChildName -like '*BF097F77*' } | Select-Object -First 1
$installed = $entry.GetValue('InstallLocation').TrimEnd('\')
Write-Host "Installed at $installed"

$running = Start-Process -FilePath "$installed\TelescopeDesktop.exe" -ArgumentList '--after-update', '--moved-from', "`"$downloads`"" -PassThru
for ($i = 0; $i -lt 40 -and (Test-Path "$downloads\TelescopeDesktop.exe"); $i++) { Start-Sleep -Seconds 1 }
$left = (Get-ChildItem $downloads -Force | ForEach-Object Name | Sort-Object) -join ', '
if ($left -ne 'holiday.jpg, platform-tools') { $problems += "after the move Downloads holds: $left" }
if (-not (Test-Path "$downloads\platform-tools\fastboot.exe")) { $problems += "the move removed the user's own platform-tools" }
if ($running.HasExited) { $problems += "the installed copy exited with $($running.ExitCode)" }
else { Stop-Process -Id $running.Id -Force }

$p = Start-Process -FilePath "$installed\unins000.exe" -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
for ($i = 0; $i -lt 60 -and (Test-Path $installed); $i++) { Start-Sleep -Seconds 1 }
Remove-Item -Recurse -Force $downloads

if ($problems) {
    $problems | ForEach-Object { Write-Host "::error::$_" }
    exit 1
}
Write-Host 'Moving an unzipped copy to the installed one works.'
