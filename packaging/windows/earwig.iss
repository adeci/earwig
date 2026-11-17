; Per-user installer for earwig: no administrator rights, installs to
; %LOCALAPPDATA%\Programs\earwig, a Start menu entry, optional start at
; sign-in. Build with `iscc /DVersion=<x.y.z> /DStage=<dir> earwig.iss`
; (package.ps1 does), where <dir> holds earwig.exe, earwigw.exe and vulkan-1.dll;
; with sign.ps1's `-Iscc` arguments, Setup and Uninstall are signed too.

#ifndef Version
  #error Pass /DVersion=x.y.z
#endif
#ifndef Stage
  #error Pass /DStage=<folder with the binaries>
#endif

; Windows' file version is digits and periods only: x.y.z for x.y.z-rc.1.
#if Pos("-", Version) > 0
  #define NumericVersion Copy(Version, 1, Pos("-", Version) - 1)
#else
  #define NumericVersion Version
#endif

[Setup]
AppId={{6A3C4F2E-8F1B-4C57-9E0D-2B7E5A1D9C41}
AppName=earwig
AppVersion={#Version}
VersionInfoVersion={#NumericVersion}
VersionInfoTextVersion={#Version}
VersionInfoProductTextVersion={#Version}
AppPublisher=adeci
AppPublisherURL=https://github.com/adeci/earwig
DefaultDirName={localappdata}\Programs\earwig
DefaultGroupName=earwig
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputBaseFilename=earwig-{#Version}-windows-x64-setup
OutputDir=.
SetupIconFile=..\..\assets\earwig.ico
UninstallDisplayIcon={app}\earwig.exe
LicenseFile=..\..\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; No Restart Manager: it closes whatever has one of the files open, a console program by sending
; Ctrl+C to its whole console. PrepareToInstall stops earwig's own programs instead.
CloseApplications=no
; The build machine's clock is UTC; without this, installed files are dated in its local time.
TimeStampsInUTC=yes
#ifdef SignTool
SignTool={#SignTool}
#endif

[Tasks]
Name: autostart; Description: "Start earwig when I sign in"

[Files]
Source: "{#Stage}\earwig.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\earwigw.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\vulkan-1.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"
Source: "..\..\assets\fonts\LICENSE-IBM-Plex-Sans.txt"; DestDir: "{app}"
Source: "..\..\assets\cursors\LICENSE-Lucide.txt"; DestDir: "{app}"
Source: "LICENSE-Vulkan-Loader.txt"; DestDir: "{app}"
Source: "..\..\packaging\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"
Source: "..\..\README.md"; DestDir: "{app}"

[Icons]
Name: "{userprograms}\earwig"; Filename: "{app}\earwigw.exe"; Parameters: "settings"

[Registry]
; The same entry the settings window's "Start at sign-in" switch writes. A silent upgrade (earwig's
; own update, winget) leaves it as the user last set it there, whatever the first install chose.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; \
  ValueName: "earwig"; ValueData: """{app}\earwigw.exe"""; Tasks: autostart; Flags: uninsdeletevalue; \
  Check: not SilentUpgrade
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "earwig"; \
  Flags: dontcreatekey uninsdeletevalue

[Run]
Filename: "{app}\earwigw.exe"; Parameters: "settings"; Description: "Start earwig"; \
  Flags: nowait postinstall skipifsilent; Check: not WasRunning
; An upgrade brings back the earwig it stopped, silent ones (winget) too.
Filename: "{app}\earwigw.exe"; Flags: nowait; Check: WasRunning

[Code]
const
  PROCESS_TERMINATE = $0001;
  PROCESS_QUERY_LIMITED_INFORMATION = $1000;
  SYNCHRONIZE = $00100000;

var
  Upgrade: Boolean;
  Running: Boolean;
  Installed: Boolean;

function OpenProcess(Access: DWORD; Inherit: Boolean; ProcessId: DWORD): UINT_PTR;
  external 'OpenProcess@kernel32.dll stdcall';
function QueryFullProcessImageName(Process: UINT_PTR; Flags: DWORD; Name: String; var Size: DWORD): Boolean;
  external 'QueryFullProcessImageNameW@kernel32.dll stdcall';
function TerminateProcess(Process: UINT_PTR; Code: UINT): Boolean;
  external 'TerminateProcess@kernel32.dll stdcall';
function WaitForSingleObject(Handle: UINT_PTR; Milliseconds: DWORD): DWORD;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: UINT_PTR): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';

function Earwig(const Args: String): Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{app}\earwig.exe'), Args, '', SW_HIDE,
    ewWaitUntilTerminated, Code) and (Code = 0);
end;

{ Whether Path is in this install's folder, not a copy elsewhere (the portable zip). }
function InApp(const Path: String): Boolean;
begin
  Result := CompareText(ExtractFilePath(Path), AddBackslash(ExpandConstant('{app}'))) = 0;
end;

{ The processes running this install's earwig.exe or earwigw.exe: the daemon, its launcher and
  any command, `earwig events` in a terminal, say. }
function AppProcesses: TArrayOfInteger;
var
  Wmi, Service, Found, Process: Variant;
  I, Count, N: Integer;
begin
  SetArrayLength(Result, 0);
  try
    Wmi := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Wmi.ConnectServer('.', 'root\CIMV2');
    Found := Service.ExecQuery('SELECT ProcessId, ExecutablePath FROM Win32_Process ' +
      'WHERE Name = ''earwig.exe'' OR Name = ''earwigw.exe''');
    Count := Found.Count;
    for I := 0 to Count - 1 do
    begin
      Process := Found.ItemIndex(I);
      if not VarIsNull(Process.ExecutablePath) then
        if InApp(Process.ExecutablePath) then
        begin
          N := GetArrayLength(Result);
          SetArrayLength(Result, N + 1);
          Result[N] := Process.ProcessId;
        end;
    end;
  except
    Log('Listing earwig''s processes failed: ' + GetExceptionMessage);
  end;
end;

{ Stops this install's earwig so its files can be replaced or deleted: asks the daemon to quit,
  gives its programs Seconds to end (a terminal's `earwig events` ends with the daemon) and ends
  the rest, nothing else. Whether the daemon was running. }
function StopEarwig(Seconds: Integer): Boolean;
var
  Ids: TArrayOfInteger;
  Tries, I: Integer;
  Process: UINT_PTR;
  Path: String;
  Size: DWORD;
begin
  Result := Earwig('quit');
  Ids := AppProcesses;
  Tries := 0;
  while (GetArrayLength(Ids) > 0) and (Tries < Seconds * 4) do
  begin
    Sleep(250);
    Tries := Tries + 1;
    Ids := AppProcesses;
  end;
  for I := 0 to GetArrayLength(Ids) - 1 do
  begin
    Process := OpenProcess(PROCESS_TERMINATE or PROCESS_QUERY_LIMITED_INFORMATION or SYNCHRONIZE, False, Ids[I]);
    if Process = 0 then
      Continue;
    { Still that program: an id is reused once its process is gone. }
    Path := StringOfChar(#0, 1024);
    Size := 1024;
    if QueryFullProcessImageName(Process, 0, Path, Size) and InApp(Copy(Path, 1, Size)) then
    begin
      Log(Format('Ending %s (process %d).', [Copy(Path, 1, Size), Ids[I]]));
      TerminateProcess(Process, 1);
      WaitForSingleObject(Process, 5000);
    end;
    CloseHandle(Process);
  end;
end;

{ An upgrade stops the running earwig first, so its files can be replaced. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  Upgrade := FileExists(ExpandConstant('{app}\earwig.exe'));
  if Upgrade then
    Running := StopEarwig(10);
end;

function WasRunning: Boolean;
begin
  Result := Running;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    StopEarwig(10);
end;

function SilentUpgrade: Boolean;
begin
  Result := Upgrade and WizardSilent;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    Installed := True;
end;

{ An upgrade that stopped earwig and then failed starts it again; files the setup had
  already replaced stay replaced. }
procedure DeinitializeSetup;
var
  Code: Integer;
begin
  if Running then
    if not Installed then
      Exec(ExpandConstant('{app}\earwigw.exe'), '', '', SW_SHOWNORMAL, ewNoWait, Code);
end;
