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
Compression=lzma2/max
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
Name: "{autoprograms}\Telescope"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\Telescope"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

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

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Settings: String;
begin
  if CurUninstallStep = usUninstall then
  begin
    CloseRunningCopies();  // after "Are you sure?", so saying no there leaves Telescope running
    RemoveStartAtSignIn();
  end;
  if CurUninstallStep = usPostUninstall then
  begin
    // Settings and pairings stay unless asked, so a reinstall picks up where it left off
    Settings := ExpandConstant('{userappdata}\Telescope');
    if DirExists(Settings) and not UninstallSilent() then
      if MsgBox('Also delete your Telescope settings and paired phones?' + #13#10#13#10 +
                'Keep them if you might install Telescope again.',
                mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(Settings, True, True, True);
  end;
end;
