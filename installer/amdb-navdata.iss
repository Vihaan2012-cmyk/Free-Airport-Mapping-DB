; AMDB Navdata installer (Inno Setup 6): the navigation-data converter on its own.
; Build with:  python tools/make_installer.py
; which compiles the binaries first and passes the version in as AppVersion.
;
; Its own AppId, so it installs and uninstalls beside AMDB Bridge rather than replacing
; it: someone may reasonably want the converter, the bridge, or both.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "AMDB Navdata"
#define AppExe "AMDB Navdata.exe"
#define Root ".."

[Setup]
AppId={{2F7A1C94-8D6B-4A3E-B012-5C9E7D4F8B31}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Free Airport Mapping DB
AppPublisherURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB
AppSupportURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/issues
AppUpdatesURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/releases
VersionInfoVersion={#AppVersion}
; Installs for the current user, so Windows does not ask for administrator rights: the
; converter writes to an aircraft's own folder and needs nothing of the system.
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
OutputBaseFilename=AMDB-Navdata-Setup-{#AppVersion}
Compression=lzma2/ultra64
SolidCompression=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "Add amdb-navdata to the command line, so it can be run from any folder"; GroupDescription: "Command line:"

[Files]
Source: "{#Root}\target\release\amdb-navdata-gui.exe"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion
Source: "{#Root}\target\release\amdb-navdata.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "{#Root}\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; The command-line copy on PATH, for the user who ticked that. `uninsdeletevalue` takes
; the entry away again, and Inno appends rather than replacing what is already there.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
    Check: NeedsPath(ExpandConstant('{app}')); Tasks: addtopath; Flags: preservestringtype

[Run]
Filename: "{app}\{#AppExe}"; Description: "Open {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
{ True when this folder is not already on the user's PATH, so ticking the task twice does
  not put it there twice. }
function NeedsPath(Dir: string): Boolean;
var
  Existing: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Existing) then
    Existing := '';
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Existing) + ';') = 0;
end;
