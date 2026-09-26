; A320 OANS installer (Inno Setup 6): the airport moving map for the Fenix A320, on its own.
; Build with:  python tools/make_installer.py
; which compiles the binaries first and passes the version in as AppVersion.
;
; Its own AppId, so it installs and uninstalls beside AMDB Bridge rather than replacing
; it. AMDB Bridge carries the same A320 OANS; this is for people who want only that.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "A320 OANS"
#define AppExe "A320 OANS.exe"
#define Root ".."

[Setup]
AppId={{8C3E51A7-4F2B-4D9A-B6E0-71A5D2C9F413}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Free Airport Mapping DB
AppPublisherURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB
AppSupportURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/issues
AppUpdatesURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/releases
VersionInfoVersion={#AppVersion}
; Installs for the current user, so Windows does not ask for administrator rights: the
; A320 OANS needs no hosts-file redirect and no certificate.
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
; The display in the package is FlyByWire's GPL code; the program serving it is MIT.
LicenseFile={#Root}\packages\msfs-a320-oans\LICENSE.txt
InfoBeforeFile={#Root}\packages\msfs-a320-oans\README.txt
OutputDir={#Root}\dist
OutputBaseFilename=A320-OANS-Setup-{#AppVersion}
Compression=lzma2/ultra64
SolidCompression=yes
; A copy left running in the notification area is asked to quit first (see [Code]); this
; is the fallback that tells the user if one will not.
AppMutex=A320OANS.Instance
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startsim"; Description: "Start A320 OANS with Microsoft Flight Simulator (the map needs it running)"; GroupDescription: "Starting up:"

[Files]
Source: "{#Root}\target\release\a320-oans.exe"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion
Source: "{#Root}\packages\msfs-a320-oans\*"; DestDir: "{app}\msfs\amdb-a320-oans"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Root}\packages\msfs-a320-oans\README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\packages\msfs-a320-oans\LICENSE.txt"; DestDir: "{app}"; DestName: "LICENSE-A320-OANS-GPL.txt"; Flags: ignoreversion
Source: "{#Root}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE-AMDB-Bridge-MIT.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"

[Run]
Filename: "{app}\{#AppExe}"; Parameters: "--install"; StatusMsg: "Adding the A320 OANS to the Fenix A320..."; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExe}"; Parameters: "--start-with-sim on"; Tasks: startsim; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExe}"; Description: "Start {#AppName} now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Takes the A320 OANS out of every simulator, puts the Fenix's files back, and removes its
; simulator start-up entry.
Filename: "{app}\{#AppExe}"; Parameters: "--uninstall"; RunOnceId: "A320OANSCleanup"; Flags: runhidden waituntilterminated

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
