; AMDB Airport Map installer (Inno Setup 6): the OANS toolbar window for Microsoft Flight
; Simulator, on its own. Build with:  python tools/make_installer.py
; which passes the package's version in as AppVersion.
;
; It carries no bridge. It asks where AMDB Bridge or the A320 OANS program is installed,
; keeps the package in its own folder, and has that program put it into each simulator's
; Community folder (the program knows where they are, including ones chosen by hand).
; The Community folders it went into are written beside the package, so uninstalling
; takes it out of them without needing the bridge.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "AMDB Airport Map"
#define Root ".."
#define Package "amdb-oans-toolbar"
; The bridges this works with, as their installers register them, and the first version
; of each that can install the Airport Map (--install-toolbar).
#define BridgeKey "Software\Microsoft\Windows\CurrentVersion\Uninstall\{6B2E8F3A-4C1D-4E7B-9A55-3F0C2D8E1A47}_is1"
#define BridgeExe "AMDB Bridge.exe"
#define BridgeMin "1.2.7"
#define OansKey "Software\Microsoft\Windows\CurrentVersion\Uninstall\{8C3E51A7-4F2B-4D9A-B6E0-71A5D2C9F413}_is1"
#define OansExe "A320 OANS.exe"
#define OansMin "1.0.5"

[Setup]
AppId={{3F6A9C21-8B4E-4D7A-A1C5-92E07B5D4F18}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Free Airport Mapping DB
AppPublisherURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB
AppSupportURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/issues
AppUpdatesURL=https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/releases
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
SetupIconFile={#Root}\assets\amdb-bridge.ico
UninstallDisplayName={#AppName}
; The window is FlyByWire's GPL code.
LicenseFile={#Root}\packages\msfs-amdb-oans-toolbar\LICENSE.txt
InfoBeforeFile={#Root}\packages\msfs-amdb-oans-toolbar\README.txt
OutputDir={#Root}\dist
OutputBaseFilename=AMDB-Airport-Map-Setup-{#AppVersion}
Compression=lzma2/ultra64
SolidCompression=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#Root}\packages\msfs-amdb-oans-toolbar\*"; DestDir: "{app}\msfs\{#Package}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Root}\packages\msfs-amdb-oans-toolbar\README.txt"; DestDir: "{app}"; Flags: ignoreversion

[INI]
Filename: "{app}\bridge.ini"; Section: "Bridge"; Key: "Folder"; String: "{code:BridgeDir}"; Flags: uninsdeletesection

[UninstallDelete]
Type: files; Name: "{app}\msfs\community-folders.txt"
Type: files; Name: "{app}\msfs\install-problems.txt"
Type: files; Name: "{app}\bridge.ini"

[Code]
var
  BridgePage: TInputDirWizardPage;

{ A dotted version as numbers, compared part by part: -1, 0 or 1. }
function CompareVersions(A, B: String): Integer;
var
  PA, PB, NA, NB: Integer;
begin
  Result := 0;
  while (Result = 0) and ((A <> '') or (B <> '')) do
  begin
    PA := Pos('.', A);
    PB := Pos('.', B);
    if PA = 0 then begin NA := StrToIntDef(A, 0); A := ''; end
    else begin NA := StrToIntDef(Copy(A, 1, PA - 1), 0); A := Copy(A, PA + 1, Length(A)); end;
    if PB = 0 then begin NB := StrToIntDef(B, 0); B := ''; end
    else begin NB := StrToIntDef(Copy(B, 1, PB - 1), 0); B := Copy(B, PB + 1, Length(B)); end;
    if NA < NB then Result := -1
    else if NA > NB then Result := 1;
  end;
end;

{ A value of an installed program's uninstall entry, for this user or for everyone. }
function Registered(Key, Name: String; var Value: String): Boolean;
begin
  Result := RegQueryStringValue(HKCU, Key, Name, Value) or RegQueryStringValue(HKLM, Key, Name, Value);
end;

function SameFolder(A, B: String): Boolean;
begin
  Result := CompareText(RemoveBackslashUnlessRoot(A), RemoveBackslashUnlessRoot(B)) = 0;
end;

{ Where a bridge is installed, if one is. AMDB Bridge first: it is the fuller one. }
function FoundBridge(): String;
var
  Dir: String;
begin
  if Registered('{#BridgeKey}', 'InstallLocation', Dir) and FileExists(AddBackslash(Dir) + '{#BridgeExe}') then
    Result := Dir
  else if Registered('{#OansKey}', 'InstallLocation', Dir) and FileExists(AddBackslash(Dir) + '{#OansExe}') then
    Result := Dir
  else
    Result := ExpandConstant('{autopf}\AMDB Bridge');
end;

function BridgeDir(Param: String): String;
begin
  Result := RemoveBackslashUnlessRoot(BridgePage.Values[0]);
end;

{ The program in the chosen folder, or '' if neither is there. }
function BridgeProgram(Dir: String): String;
begin
  if FileExists(AddBackslash(Dir) + '{#BridgeExe}') then
    Result := AddBackslash(Dir) + '{#BridgeExe}'
  else if FileExists(AddBackslash(Dir) + '{#OansExe}') then
    Result := AddBackslash(Dir) + '{#OansExe}'
  else
    Result := '';
end;

{ '' when the program in Dir can install the Airport Map, or why not. A copy its
  installer did not register (a build folder) is taken as it is. }
function TooOld(Dir: String): String;
var
  Where, Version: String;
begin
  Result := '';
  if FileExists(AddBackslash(Dir) + '{#BridgeExe}') then
  begin
    if Registered('{#BridgeKey}', 'InstallLocation', Where) and SameFolder(Where, Dir)
      and Registered('{#BridgeKey}', 'DisplayVersion', Version) and (CompareVersions(Version, '{#BridgeMin}') < 0) then
      Result := 'This is AMDB Bridge ' + Version + '. The Airport Map needs AMDB Bridge {#BridgeMin} or later: update it first.';
  end
  else if Registered('{#OansKey}', 'InstallLocation', Where) and SameFolder(Where, Dir)
    and Registered('{#OansKey}', 'DisplayVersion', Version) and (CompareVersions(Version, '{#OansMin}') < 0) then
    Result := 'This is A320 OANS ' + Version + '. The Airport Map needs A320 OANS {#OansMin} or later: update it first.';
end;

procedure InitializeWizard();
begin
  BridgePage := CreateInputDirPage(wpSelectDir,
    'Where is your bridge?',
    'The Airport Map gets its airport maps and taxi routes from AMDB Bridge or the A320 OANS program, running on this computer while you fly.',
    'Choose the folder AMDB Bridge or the A320 OANS is installed in (the one with "{#BridgeExe}" or "{#OansExe}" in it). ' +
      'It puts the Airport Map into each simulator''s Community folder. Neither is included here: get one from ' +
      'github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB.',
    False, '');
  BridgePage.Add('Bridge folder:');
  BridgePage.Values[0] := GetPreviousData('BridgeDir', FoundBridge());
end;

procedure RegisterPreviousData(PreviousDataKey: Integer);
begin
  SetPreviousData(PreviousDataKey, 'BridgeDir', BridgeDir(''));
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Why: String;
begin
  Result := True;
  if CurPageID = BridgePage.ID then
  begin
    if BridgeProgram(BridgeDir('')) = '' then
    begin
      MsgBox('Neither "{#BridgeExe}" nor "{#OansExe}" is in' + #13#10 + BridgeDir('') + #13#10#13#10 +
        'Choose the folder AMDB Bridge or the A320 OANS is installed in.', mbError, MB_OK);
      Result := False;
      Exit;
    end;
    Why := TooOld(BridgeDir(''));
    if Why <> '' then
    begin
      MsgBox(Why, mbError, MB_OK);
      Result := False;
    end;
  end;
end;

function UpdateReadyMemo(Space, NewLine, MemoUserInfoInfo, MemoDirInfo, MemoTypeInfo, MemoComponentsInfo, MemoGroupInfo, MemoTasksInfo: String): String;
begin
  Result := MemoDirInfo + NewLine + NewLine + 'Bridge:' + NewLine + Space + BridgeProgram(BridgeDir(''));
end;

{ Have the bridge put the package into each simulator, and wait for its answer: a list of
  the Community folders it went into, or what went wrong. A bridge too old to know the
  request would just start instead, so it is not waited on for ever. }
procedure AddToSimulators();
var
  Msfs, RecordFile, Problems, Exe: String;
  Code, Waited: Integer;
  Text: AnsiString;
begin
  Msfs := ExpandConstant('{app}\msfs');
  RecordFile := Msfs + '\community-folders.txt';
  Problems := Msfs + '\install-problems.txt';
  DeleteFile(Problems);
  DeleteFile(RecordFile);
  Exe := BridgeProgram(BridgeDir(''));
  WizardForm.StatusLabel.Caption := 'Adding the Airport Map to Microsoft Flight Simulator...';
  if not Exec(Exe, '--install-toolbar "' + Msfs + '\{#Package}"', '', SW_HIDE, ewNoWait, Code) then
  begin
    MsgBox('Could not start ' + Exe + ': ' + SysErrorMessage(Code), mbError, MB_OK);
    Exit;
  end;
  Waited := 0;
  while not FileExists(RecordFile) and not FileExists(Problems) and (Waited < 60000) do
  begin
    Sleep(250);
    Waited := Waited + 250;
  end;
  if FileExists(Problems) then
  begin
    LoadStringFromFile(Problems, Text);
    MsgBox('The Airport Map was not added to the simulator:' + #13#10#13#10 + String(Text), mbError, MB_OK);
  end
  else if not FileExists(RecordFile) then
    MsgBox(ExtractFileName(Exe) + ' did not answer. It may be older than AMDB Bridge {#BridgeMin} or A320 OANS {#OansMin}: ' +
      'update it, then run this installer again.', mbError, MB_OK);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    AddToSimulators();
end;

{ Take the package out of every Community folder the bridge put it into. Only a folder
  with the package's own name and manifest is removed. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Lines: TArrayOfString;
  I: Integer;
  Dir: String;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  if not LoadStringsFromFile(ExpandConstant('{app}\msfs\community-folders.txt'), Lines) then
    Exit;
  for I := 0 to GetArrayLength(Lines) - 1 do
  begin
    if Trim(Lines[I]) <> '' then
    begin
      Dir := AddBackslash(Trim(Lines[I])) + '{#Package}';
      if FileExists(Dir + '\manifest.json') then
        DelTree(Dir, True, True, True);
    end;
  end;
end;
