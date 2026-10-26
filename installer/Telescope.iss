; Telescope's Windows setup: a per-user install of the same folder Telescope-windows.zip holds.
; Build: installer/build.ps1 (it passes the defines below), or by hand:
;   iscc /DAppVersion=3.3.1 /DBuild=0 /DSourceDir=..\bundle /DOutputDir=..\out installer\Telescope.iss
;
; Per-user on purpose: the app updates itself by swapping files in its own folder (desktop/updates.py), so the
; folder has to be writable without admin. {autopf} is %LOCALAPPDATA%\Programs here. Program Files\Telescope
; belongs to the camera driver, which the app's first-run checklist installs with its own UAC prompt.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif
#ifndef Build
  #define Build "0"
#endif
#ifndef SourceDir
  #define SourceDir "..\bundle"
#endif
#ifndef OutputDir
  #define OutputDir "..\out"
#endif

#define AppExe "TelescopeDesktop.exe"

[Setup]
; Never change AppId: it's how Windows and the next setup find this install.
AppId={{BF097F77-F8B9-4F70-B23C-0B500DB1A07C}
AppName=Telescope
AppVersion={#AppVersion}
AppVerName=Telescope {#AppVersion}
VersionInfoVersion={#AppVersion}.{#Build}
AppPublisher=LunarKittyy
AppPublisherURL=https://telescope.webcam
AppSupportURL=https://github.com/LunarKittyy/Telescope/issues
AppUpdatesURL=https://github.com/LunarKittyy/Telescope/releases
PrivilegesRequired=lowest
DefaultDirName={autopf}\Telescope
DisableProgramGroupPage=yes
; Always its own folder: the uninstaller clears what the app adds there by name (lib-*, below), which in a folder
; someone picked, like C:\Android, could be theirs
DisableDirPage=yes
UsePreviousAppDir=yes
; x64compatible also lets Windows on Arm install it (it runs the x64 app emulated); older Inno only knows x64
#if Ver >= EncodeVer(6, 3, 0)
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#else
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
#endif
MinVersion=10.0
SetupIconFile=..\desktop\resources\telescope.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName=Telescope
WizardStyle=modern
; CI's pull request check only installs and uninstalls it, so it passes /DFast to skip the slow squeeze
#ifdef Fast
Compression=lzma2/fast
#else
Compression=lzma2/max
#endif
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=TelescopeSetup
; Telescope running from this folder gets asked to close (Restart Manager), so its files can be replaced
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: desktopicon; Description: "Put Telescope on the desktop"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
; The app takes this id too (windows.use_installed_app_id), so its notifications say Telescope, not the exe's name
Name: "{autoprograms}\Telescope"; Filename: "{app}\{#AppExe}"; AppUserModelID: "LunarKittyy.Telescope"
Name: "{autodesktop}\Telescope"; Filename: "{app}\{#AppExe}"; AppUserModelID: "LunarKittyy.Telescope"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "Open Telescope"; Flags: nowait postinstall skipifsilent

; What the app adds after setup, which the uninstall log can't know about: libraries from in-app updates, the updater's
; working files (desktop/update_guard.py) and the log shortcut (desktop/telescope/diagnostics.py).
[UninstallDelete]
Type: filesandordirs; Name: "{app}\lib-*"
Type: filesandordirs; Name: "{app}\.update-staging"
Type: filesandordirs; Name: "{app}\.previous"
Type: files; Name: "{app}\.update.json"
Type: files; Name: "{app}\.update.json.tmp"
Type: files; Name: "{app}\.update-failed"
Type: files; Name: "{app}\.moved-from"
Type: files; Name: "{app}\.moved-from.tmp"
Type: files; Name: "{app}\TelescopeDesktop.old.exe"
Type: files; Name: "{app}\TelescopeDesktop.failed.exe"
Type: files; Name: "{app}\telescope.log"
; adb as the app downloads it (desktop/telescope/platform/adb_download.py), and a download cut short
Type: filesandordirs; Name: "{localappdata}\Telescope\platform-tools"
Type: filesandordirs; Name: "{localappdata}\Telescope\.adb-*"
Type: dirifempty; Name: "{localappdata}\Telescope"
Type: filesandordirs; Name: "{app}\unitycapture"
Type: dirifempty; Name: "{app}"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  RunValue = 'Telescope';  // desktop/telescope/platform/autostart.py

// "Open at sign-in" points the Run key at this copy's exe. Once it's gone that entry would only fail at every
// sign-in, so take it out, but leave one that points at some other copy of Telescope alone.
procedure RemoveStartAtSignIn();
var
  Cmd: String;
begin
  if RegQueryStringValue(HKCU, RunKey, RunValue, Cmd) then
    if Pos(Lowercase(ExpandConstant('{app}\{#AppExe}')), Lowercase(Cmd)) > 0 then
      RegDeleteValue(HKCU, RunKey, RunValue);
end;

// The processes running from Exe (a full path), through WMI; Stop ends them. -1 when WMI can't be asked.
function RunningFrom(Exe: String; Stop: Boolean): Integer;
var
  Locator, Wmi, Procs, Proc: Variant;
  I: Integer;
begin
  Result := 0;
  StringChangeEx(Exe, '\', '\\', True);  // WQL string escapes
  StringChangeEx(Exe, '''', '\''', True);
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Wmi := Locator.ConnectServer('.', 'root\CIMV2');
    Procs := Wmi.ExecQuery('SELECT ProcessId FROM Win32_Process WHERE ExecutablePath = ''' + Exe + '''');
    Result := Procs.Count;
    if Stop then
      for I := 0 to Procs.Count - 1 do
      begin
        Proc := Procs.ItemIndex(I);
        Proc.Terminate(0);
      end;
  except
    Log('Couldn''t look for running processes: ' + GetExceptionMessage);
    Result := -1;
  end;
end;

// Restart Manager only closes apps during setup: close a running Telescope and Get adb's adb server, or files stay
procedure CloseRunningCopies();
var
  App, Adb: String;
  I: Integer;
begin
  App := ExpandConstant('{app}\{#AppExe}');
  Adb := ExpandConstant('{localappdata}\Telescope\platform-tools\adb.exe');
  RunningFrom(App, True);
  RunningFrom(Adb, True);
  for I := 1 to 50 do
  begin
    if (RunningFrom(App, False) <= 0) and (RunningFrom(Adb, False) <= 0) then
      Exit;
    Sleep(100);
  end;
end;

function PsQuote(Value: String): String;
begin
  StringChangeEx(Value, '''', '''''', True);
  Result := '''' + Value + '''';
end;

// The virtual camera the app installs for every account (desktop/telescope/platform/windows.py): UnityCapture, or on
// Windows 11 its own (desktop/virtualcamera)
function CameraInstalled(): Boolean;
begin
  Result := DirExists(ExpandConstant('{commonpf64}\Telescope\UnityCapture')) or
            DirExists(ExpandConstant('{commonpf64}\Telescope\VirtualCamera')) or
            RegKeyExists(HKLM64, 'SOFTWARE\Classes\CLSID\{5C2CD55C-92AD-4999-8666-912BD3E70010}') or
            RegKeyExists(HKLM64, 'SOFTWARE\Classes\CLSID\{D1063481-2B46-4750-8A9D-85F69573355A}');
end;

// As admin: unregister the cameras and delete Program Files\Telescope (Camera), and this copy's firewall rules
procedure RunAsAdmin(Camera: Boolean);
var
  Script: String;
  ResultCode: Integer;
begin
  Script := '';
  if Camera then
    // Files a camera app still has open go at the next restart, the way setups replace locked files
    Script := '$uc = ' + PsQuote(ExpandConstant('{commonpf64}\Telescope')) + '; ' +
      '$rs = Join-Path $env:windir ''Sysnative\regsvr32.exe''; if (-not (Test-Path -LiteralPath $rs)) { $rs = Join-Path $env:windir ''System32\regsvr32.exe'' }; ' +
      'foreach ($n in ''UnityCapture\UnityCaptureFilter64.dll'', ''UnityCapture\UnityCaptureFilter32.dll'', ''VirtualCamera\TelescopeCamera.dll'') { ' +
      '$d = Join-Path $uc $n; if (Test-Path -LiteralPath $d) { & $rs /s /u $d | Out-Null } }; ' +
      'Remove-Item -LiteralPath $uc -Recurse -Force -ErrorAction SilentlyContinue; ' +
      'if (Test-Path -LiteralPath $uc) { ' +
      '$k = ''HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager''; $p = @(); ' +
      '$old = (Get-ItemProperty -LiteralPath $k -ErrorAction SilentlyContinue).PendingFileRenameOperations; if ($old) { $p += $old }; ' +
      '$left = @(Get-ChildItem -LiteralPath $uc -Recurse -Force | Sort-Object { $_.FullName.Length } -Descending) + @(Get-Item -LiteralPath $uc); ' +
      'foreach ($i in $left) { $p += ''\??\'' + $i.FullName; $p += '''' }; ' +
      'Set-ItemProperty -LiteralPath $k -Name PendingFileRenameOperations -Type MultiString -Value ([string[]]$p) }; ';
  Script := Script +
    '$ours = @(' + PsQuote(ExpandConstant('{app}\{#AppExe}')) + ', ' +
    PsQuote(ExpandConstant('{localappdata}\Telescope\platform-tools\adb.exe')) + '); ' +
    'Get-NetFirewallApplicationFilter | Where-Object { $ours -contains $_.Program } | Get-NetFirewallRule | ' +
    'Remove-NetFirewallRule';
  // Not {sysnative}: UAC can't find that path. This PowerShell is 32-bit like the uninstaller, hence {commonpf64} and Sysnative regsvr32
  repeat
    if ShellExec('runas', ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
                 '-NoProfile -NonInteractive -Command "' + Script + '"', '', SW_HIDE, ewWaitUntilTerminated,
                 ResultCode) then
      Exit;
    Log('The admin clean-up didn''t run: ' + SysErrorMessage(ResultCode));
  until not Camera or UninstallSilent() or
        (MsgBox('The Telescope camera is still installed because Windows didn''t get admin permission to remove it.' + #13#10#13#10 +
                'You can try again, or remove it later by installing Telescope again and uninstalling it.',
                mbError, MB_RETRYCANCEL) <> IDRETRY);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Settings, Adb: String;
  Camera: Boolean;
begin
  if CurUninstallStep = usUninstall then
  begin
    CloseRunningCopies();  // after "Are you sure?", so saying no there leaves Telescope running
    RemoveStartAtSignIn();
    Camera := False;
    if CameraInstalled() then
      // Silent only as admin already: an admin prompt out of nowhere would be worse than leaving it
      if UninstallSilent() then
        Camera := IsAdmin()
      else
        Camera := MsgBox('Also remove the Telescope virtual camera?' + #13#10#13#10 +
                         'It''s installed for everyone on this computer which is why Windows will ask for admin permission.',
                         mbConfirmation, MB_YESNO) = IDYES;
    // Firewall rules need admin too: removed along with the camera, or anyway when that needs no prompt
    if Camera or IsAdmin() then
      RunAsAdmin(Camera);
  end;
  if CurUninstallStep = usPostUninstall then
  begin
    DelTree(GetEnv('TEMP') + '\Telescope', True, True, True);  // the log (desktop/telescope/diagnostics.py)
    // adb still locked (a server that outlived Telescope): delete it at the next sign-in, which needs no admin
    Adb := ExpandConstant('{localappdata}\Telescope');
    if DirExists(Adb + '\platform-tools') then
      RegWriteStringValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\RunOnce', 'TelescopeCleanup',
                          'cmd.exe /d /c "rd /s /q "' + Adb + '\platform-tools" & rd "' + Adb + '""');
    // Settings and pairings stay unless asked, so a reinstall picks up where it left off
    Settings := ExpandConstant('{userappdata}\Telescope');
    if DirExists(Settings) and not UninstallSilent() then
      if MsgBox('Also delete your Telescope settings and paired phones?' + #13#10#13#10 +
                'Keep them if you might install Telescope again.',
                mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(Settings, True, True, True);
  end;
end;
