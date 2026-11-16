; Inno Setup script for Norrlog's Windows installer. The release workflow runs:
;
;   ISCC /DAppVersion=0.1.0 /DStage=<folder with norrlog.exe, norrlogd.exe and docs> /O<out> norrlog.iss
;
; Installs per user by default (no admin prompt), into
; %LOCALAPPDATA%\Programs\Norrlog, with an option to install for all users.
; Adds a Start menu entry, an optional desktop shortcut, and an uninstaller
; listed in Settings > Apps. Adapted from Bard's (github.com/hammackj/bard).

#ifndef AppVersion
  #error Pass /DAppVersion=X.Y.Z
#endif
#ifndef Stage
  #error Pass /DStage=<folder with norrlog.exe and norrlogd.exe>
#endif

[Setup]
; Never change AppId: upgrades and the uninstaller find Norrlog by it.
AppId={{300C2D82-DAB5-4FB0-A65C-030E63FC43EC}
AppName=Norrlog
AppVersion={#AppVersion}
AppVerName=Norrlog {#AppVersion}
AppPublisher=hammackj
AppPublisherURL=https://github.com/hammackj/norrlog
AppSupportURL=https://github.com/hammackj/norrlog/issues
AppUpdatesURL=https://github.com/hammackj/norrlog/releases
DefaultDirName={autopf}\Norrlog
DefaultGroupName=Norrlog
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputBaseFilename=norrlog-{#AppVersion}-x86_64-windows-setup
SetupIconFile=norrlog.ico
UninstallDisplayIcon={app}\norrlog.exe
UninstallDisplayName=Norrlog
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; Closes a running Norrlog before replacing or removing it. Forced, because
; the daemon has no window to ask: it outlives the app for a minute, and for
; as long as an OBS overlay is attached.
CloseApplications=force

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Stage}\norrlog.exe"; DestDir: "{app}"; Flags: ignoreversion
; The app starts the daemon from beside itself.
Source: "{#Stage}\norrlogd.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\THIRD-PARTY-LICENSES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Norrlog"; Filename: "{app}\norrlog.exe"
Name: "{autodesktop}\Norrlog"; Filename: "{app}\norrlog.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\norrlog.exe"; Description: "{cm:LaunchProgram,Norrlog}"; Flags: nowait postinstall skipifsilent
; An update the app ran silently: start the new version when it is done.
Filename: "{app}\norrlog.exe"; Flags: nowait; Check: Relaunch

[UninstallRun]
; A daemon still running would hold norrlogd.exe open. It saves as it goes,
; so stopping it loses nothing.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM norrlogd.exe"; Flags: runhidden; RunOnceId: "StopDaemon"

[Code]
// Run by the app's updater as /VERYSILENT /RELAUNCH=1: a silent install
// skips the usual "Launch Norrlog" step, so this asks for it.
function Relaunch: Boolean;
begin
  Result := ExpandConstant('{param:RELAUNCH|0}') = '1';
end;

// Norrlog's own data lives outside the install folder: fight history and
// state in %APPDATA%\norrlog, settings in %APPDATA%\eqlparser-gui (the name
// the settings file had before the rename, kept so they carry over).
// Uninstalling keeps them unless the user says otherwise, so a reinstall
// picks up where it left off. EverQuest's own logs are never touched.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir, ConfigDir: String;
begin
  if (CurUninstallStep <> usPostUninstall) or UninstallSilent then
    exit;
  DataDir := ExpandConstant('{userappdata}\norrlog');
  ConfigDir := ExpandConstant('{userappdata}\eqlparser-gui');
  if MsgBox('Also delete your Norrlog fight history and settings?' + #13#10#13#10 +
            'They are in ' + DataDir + ' and ' + ConfigDir + '. EverQuest''s own logs are not touched.' + #13#10#13#10 +
            'Choose No to keep them for a later reinstall.',
            mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
  begin
    DelTree(DataDir, True, True, True);
    DelTree(ConfigDir, True, True, True);
  end;
end;
