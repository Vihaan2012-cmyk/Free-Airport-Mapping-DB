; A330 OANS installer (Inno Setup 6): the airport moving map for the Headwind A330, on its own.
; Build with:  python tools/make_installer.py
; which compiles the binaries first and passes the version in as AppVersion.
;
; Its own AppId, so it installs and uninstalls beside AMDB Bridge and the A320 OANS rather
; than replacing either. AMDB Bridge carries the same A330 OANS; this is for people who
; want only that. Its program serves the maps itself, with the same server as theirs.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "A330 OANS"
#define AppExe "A330 OANS.exe"
#define Root ".."

[Setup]
AppId={{3F6B2D90-7A41-4E58-9C2D-A83E15F60B27}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Free Airport Mapping DB
AppPublisherURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB
AppSupportURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/issues
AppUpdatesURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/releases
VersionInfoVersion={#AppVersion}
; Installs for the current user, so Windows does not ask for administrator rights: the
; A330 OANS needs no hosts-file redirect and no certificate.
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
LicenseFile={#Root}\packages\msfs-a330-oans\LICENSE.txt
InfoBeforeFile={#Root}\packages\msfs-a330-oans\README.txt
OutputDir={#Root}\dist
OutputBaseFilename=A330-OANS-Setup-{#AppVersion}
Compression=lzma2/ultra64
SolidCompression=yes
; A copy left running in the notification area is asked to quit first (see [Code]); this
; is the fallback that tells the user if one will not.
AppMutex=A330OANS.Instance
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startsim"; Description: "Start A330 OANS with Microsoft Flight Simulator (the map needs it running)"; GroupDescription: "Starting up:"

[Files]
Source: "{#Root}\target\release\a330-oans.exe"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion
Source: "{#Root}\packages\msfs-a330-oans\*"; DestDir: "{app}\msfs\amdb-a330-oans"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Root}\packages\msfs-a330-oans\README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\packages\msfs-a330-oans\LICENSE.txt"; DestDir: "{app}"; DestName: "LICENSE-A330-OANS-GPL.txt"; Flags: ignoreversion
Source: "{#Root}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE-AMDB-Bridge-MIT.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"

[Run]
Filename: "{app}\{#AppExe}"; Parameters: "--install"; StatusMsg: "Adding the A330 OANS to the Headwind A330..."; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExe}"; Parameters: "--start-with-sim on"; Tasks: startsim; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExe}"; Description: "Start {#AppName} now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Takes the A330 OANS out of every simulator, puts the Headwind A330's files back, and removes its
; simulator start-up entry.
Filename: "{app}\{#AppExe}"; Parameters: "--uninstall"; RunOnceId: "A330OANSCleanup"; Flags: runhidden waituntilterminated

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
