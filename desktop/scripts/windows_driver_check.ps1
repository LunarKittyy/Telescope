# Native Windows checks for the virtual camera driver install, as admin with UAC off (a CI runner). Run from desktop/.
# Leaves the driver installed in Program Files. Usage: pwsh scripts/windows_driver_check.ps1
$ErrorActionPreference = 'Stop'
$programFiles = [Environment]::GetFolderPath('ProgramFiles')
$telescope = Join-Path $programFiles 'Telescope'
$dst = Join-Path $telescope 'UnityCapture'
$appDir = Join-Path (Get-Location) 'unitycapture'
$scratch = Join-Path ([IO.Path]::GetTempPath()) ('telescope-driver-check-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $scratch | Out-Null

function Register {
    $out = python -c "from telescope.platform.windows import register_unitycapture as r; ok, msg = r(); print('OK' if ok else 'FAIL', msg)"
    Write-Host "  register: $out"
    return $out
}

function Py($code) { python -c $code }

function Remove-Tree {
    if (Test-Path -LiteralPath $telescope) {
        $item = Get-Item -LiteralPath $telescope -Force
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { $item.Delete() } else { Remove-Item -LiteralPath $telescope -Recurse -Force }
    }
}

function Expect-Rejected($what, $sentinel) {
    $before = Get-Content -LiteralPath $sentinel -Raw
    $out = Register
    if ($out -notmatch '^FAIL .*delete that folder') { throw "$what was not rejected: $out" }
    if ((Get-Content -LiteralPath $sentinel -Raw) -ne $before) { throw "${what}: the link's target was changed" }
    if (Get-ChildItem -LiteralPath (Split-Path $sentinel) -Filter 'UnityCapture*.dll' -Recurse -ErrorAction SilentlyContinue | Where-Object { $_.FullName -ne $sentinel }) {
        throw "${what}: DLLs were copied through the link"
    }
    Write-Host "ok: $what is rejected and its target untouched"
}

Remove-Tree

Write-Host '1. A registration from the app folder, as Telescope 3.1.1 and older made it'
foreach ($bits in '32', '64') {
    $p = Start-Process regsvr32.exe -ArgumentList '/s', '"/i:UnityCaptureName=Telescope"', ('"' + (Join-Path $appDir "UnityCaptureFilter$bits.dll") + '"') -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "old-style regsvr32 failed: $($p.ExitCode)" }
}
if ((Py "from telescope.platform.windows import uc_in_app_folder as f; print(f())") -ne 'True') { throw 'old registration not seen as in the app folder' }
Write-Host 'ok: seen as registered from the app folder'

Write-Host '2. Program Files\Telescope is a junction to a folder a user controls'
$elsewhere = Join-Path $scratch 'junction-target'
New-Item -ItemType Directory -Path $elsewhere | Out-Null
Set-Content -LiteralPath (Join-Path $elsewhere 'sentinel.txt') -Value 'untouched'
New-Item -ItemType Junction -Path $telescope -Target $elsewhere | Out-Null
Expect-Rejected 'a junction' (Join-Path $elsewhere 'sentinel.txt')
Remove-Tree

Write-Host '3. An admin-made folder with a DLL that is a symlink to a user file'
New-Item -ItemType Directory -Path $dst | Out-Null
$userFile = Join-Path $scratch 'user.dll'
Set-Content -LiteralPath $userFile -Value 'user file'
New-Item -ItemType SymbolicLink -Path (Join-Path $dst 'UnityCaptureFilter64.dll') -Target $userFile | Out-Null
Expect-Rejected 'a DLL symlink' $userFile
Remove-Tree

Write-Host '4. A leftover tree owned by and writable for ordinary users'
New-Item -ItemType Directory -Path $dst | Out-Null
$stale = Join-Path $dst 'UnityCaptureFilter64.dll'
Set-Content -LiteralPath $stale -Value 'stale'
icacls $telescope /grant '*S-1-5-32-545:(OI)(CI)F' /T /Q | Out-Null
icacls $telescope /setowner '*S-1-5-32-545' /T /Q | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'could not set up the user-owned tree' }
Expect-Rejected 'a user-owned tree' $stale
Remove-Tree

Write-Host '5. A clean install, with %ProgramFiles% pointed somewhere else'
$fake = Join-Path $scratch 'fake-program-files'
New-Item -ItemType Directory -Path $fake | Out-Null
$saved = $env:ProgramFiles, $env:ProgramW6432
$env:ProgramFiles = $fake; $env:ProgramW6432 = $fake
try { $out = Register } finally { $env:ProgramFiles, $env:ProgramW6432 = $saved }
if ($out -notmatch '^OK') { throw "clean install failed: $out" }
if (Get-ChildItem -LiteralPath $fake -Recurse -Force) { throw 'the environment override decided where the DLLs went' }
foreach ($name in 'UnityCaptureFilter32.dll', 'UnityCaptureFilter64.dll') {
    if (-not (Test-Path -LiteralPath (Join-Path $dst $name))) { throw "$name missing from $dst" }
}
Write-Host 'ok: installed in the real Program Files, the override ignored'

$sid = [Security.Principal.SecurityIdentifier]
$trusted = @('S-1-5-32-544', 'S-1-5-18', 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
$writes = 0x2 -bor 0x4 -bor 0x10 -bor 0x40 -bor 0x100 -bor 0x10000 -bor 0x40000 -bor 0x80000 -bor 0x10000000 -bor 0x40000000
foreach ($item in @(Get-Item -LiteralPath $telescope) + @(Get-ChildItem -LiteralPath $telescope -Recurse -Force)) {
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "$($item.FullName) is a link" }
    $acl = Get-Acl -LiteralPath $item.FullName
    $owner = $acl.GetOwner($sid).Value
    if ($trusted -notcontains $owner) { throw "$($item.FullName) is owned by $owner" }
    foreach ($rule in $acl.GetAccessRules($true, $true, $sid)) {
        if ($rule.AccessControlType -ne 'Allow' -or ($rule.PropagationFlags -band [Security.AccessControl.PropagationFlags]::InheritOnly)) { continue }
        if ((([int]$rule.FileSystemRights) -band $writes) -and $trusted -notcontains $rule.IdentityReference.Value) {
            throw "$($item.FullName) lets $($rule.IdentityReference.Value) change it"
        }
    }
}
Write-Host 'ok: every file and folder is admin-owned and only admins can change it'

if ((Py "from telescope.platform.windows import uc_in_app_folder as f; print(f())") -ne 'False') { throw 'still registered from the app folder' }
$name = Py "from telescope.platform.windows import uc_registered_name as n; print(n())"
if ($name -ne 'Telescope') { throw "registered as '$name'" }
Py "import pyvirtualcam; cam = pyvirtualcam.Camera(width=640, height=480, fps=30, backend='unitycapture', device='Telescope'); print(cam.device); cam.close()"
if ($LASTEXITCODE -ne 0) { throw 'pyvirtualcam could not open the camera' }
Write-Host 'ok: the old registration moved to Program Files, apps list it as Telescope, and the camera opens'

Write-Host '6. Installing again over our own install keeps working'
$out = Register
if ($out -notmatch '^OK') { throw "reinstall failed: $out" }
Write-Host 'ok: reinstall'

Remove-Item -LiteralPath $scratch -Recurse -Force
Write-Host 'All driver checks passed'
