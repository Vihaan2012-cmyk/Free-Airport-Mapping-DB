; AMDB Bridge installer (Inno Setup 6).
; Build with:  python tools/make_installer.py
; which compiles the binaries first and passes the version in as AppVersion.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "AMDB Bridge"
#define AppExe "AMDB Bridge.exe"
#define Root ".."

[Setup]
AppId={{6B2E8F3A-4C1D-4E7B-9A55-3F0C2D8E1A47}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Free Airport Mapping DB
AppPublisherURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB
AppSupportURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/issues
AppUpdatesURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/releases
VersionInfoVersion={#AppVersion}
; Installs for the current user by default, so no administrator prompt; the page
; offering an install for all users is there for those who want it.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
SetupIconFile={#Root}\assets\amdb-bridge.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
LicenseFile={#Root}\LICENSE
OutputDir={#Root}\dist
OutputBaseFilename=AMDB-Bridge-Setup-{#AppVersion}
Compression=lzma2/ultra64
SolidCompression=yes
; A copy left running from the tray is asked to quit first (see [Code]); this is the
; fallback that tells the user if one will not.
AppMutex=AMDBBridge.Instance
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "a220map"; Description: "Install the airport moving map for the Synaptic A220 into Microsoft Flight Simulator 2020 and 2024"; GroupDescription: "Simulators:"
Name: "a350"; Description: "Set up the iniBuilds A350 and FlyByWire A380X airport maps (Windows asks for administrator permission)"; GroupDescription: "Simulators:"
Name: "startup"; Description: "Open AMDB Bridge in the notification area when Windows starts"; GroupDescription: "Starting up:"; Flags: unchecked
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Root}\target\release\amdb-bridge-gui.exe"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion
Source: "{#Root}\target\release\amdb-bridge.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\target\release\amdbgen.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\packages\msfs-a220-amm\*"; DestDir: "{app}\msfs\zzz-amdb-a220-amm"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Root}\tools\xplane\amdb_oans.lua"; DestDir: "{app}\xplane"; Flags: ignoreversion
Source: "{#Root}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Parameters: "--install-a220"; StatusMsg: "Installing the A220 moving map..."; Tasks: a220map; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExe}"; Parameters: "--setup-navigraph on"; StatusMsg: "Setting up the A350 and A380X..."; Tasks: a350; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExe}"; Parameters: "--run-at-login on"; Tasks: startup; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExe}"; Description: "Open AMDB Bridge now"; Flags: postinstall nowait skipifsilent

[UninstallRun]
; Takes the A220 map back out of every simulator (restoring any map it set aside), and
; removes the start-up entries, the A350 patch, the hosts-file redirect and certificate.
Filename: "{app}\{#AppExe}"; Parameters: "--uninstall"; RunOnceId: "AMDBBridgeCleanup"; Flags: runhidden waituntilterminated

[Code]
{ Ask a copy running in the notification area to quit, so files can be replaced. }
procedure QuitRunningCopy(Exe: String);
var
  Code: Integer;
begin
  if FileExists(Exe) then
    Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  QuitRunningCopy(ExpandConstant('{app}\{#AppExe}'));
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  QuitRunningCopy(ExpandConstant('{app}\{#AppExe}'));
  Result := True;
end;

{ Built airports can run to gigabytes; they are the user's to keep or delete. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Data: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    Data := ExpandConstant('{localappdata}\amdb-bridge');
    if DirExists(Data) and not UninstallSilent() then
      if MsgBox('Also delete AMDB Bridge''s settings and the airports it built?' + #13#10#13#10 +
                'These are in ' + Data + ' (or the folder you chose for them). Keep them if you plan to reinstall.',
                mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(Data, True, True, True);
  end;
end;
