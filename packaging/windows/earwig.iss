; Per-user installer for earwig: no administrator rights, installs to
; %LOCALAPPDATA%\Programs\earwig, a Start menu entry, optional start at
; sign-in. Build with `iscc /DVersion=<x.y.z> /DStage=<dir> earwig.iss`
; (package.ps1 does), where <dir> holds earwig.exe, earwigw.exe and vulkan-1.dll.

#ifndef Version
  #error Pass /DVersion=x.y.z
#endif
#ifndef Stage
  #error Pass /DStage=<folder with the binaries>
#endif

[Setup]
AppId={{6A3C4F2E-8F1B-4C57-9E0D-2B7E5A1D9C41}
AppName=earwig
AppVersion={#Version}
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
CloseApplications=yes
; The build machine's clock is UTC; without this, installed files are dated in its local time.
TimeStampsInUTC=yes

[Tasks]
Name: autostart; Description: "Start earwig when I sign in"

[Files]
Source: "{#Stage}\earwig.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\earwigw.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\vulkan-1.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"
Source: "..\..\assets\fonts\LICENSE-IBM-Plex-Sans.txt"; DestDir: "{app}"
Source: "LICENSE-Vulkan-Loader.txt"; DestDir: "{app}"
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
var
  Upgrade: Boolean;
  Running: Boolean;
  Installed: Boolean;

function Earwig(const Args: String): Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{app}\earwig.exe'), Args, '', SW_HIDE,
    ewWaitUntilTerminated, Code) and (Code = 0);
end;

{ An upgrade stops the running earwig first, so its files can be replaced. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Tries: Integer;
begin
  Result := '';
  Upgrade := FileExists(ExpandConstant('{app}\earwig.exe'));
  if Upgrade and Earwig('quit') then
  begin
    Running := True;
    Tries := 0;
    while Earwig('status') and (Tries < 50) do
    begin
      Sleep(100);
      Tries := Tries + 1;
    end;
    Sleep(500);
  end;
end;

function WasRunning: Boolean;
begin
  Result := Running;
end;

{ Whether the installed Name can be opened exclusively: no running earwig holds it. }
function Unlocked(const Name: String): Boolean;
var
  Stream: TFileStream;
begin
  Result := True;
  if not FileExists(ExpandConstant('{app}\' + Name)) then
    Exit;
  try
    Stream := TFileStream.Create(ExpandConstant('{app}\' + Name), fmOpenReadWrite or fmShareExclusive);
    Stream.Free;
  except
    Result := False;
  end;
end;

{ Uninstalling stops earwig and waits until its processes are gone, so its files
  can be deleted: `earwig quit` returns before the daemon has exited. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Tries, Code: Integer;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  if not Earwig('quit') then
    Exit;
  Tries := 0;
  while Earwig('status') and (Tries < 50) do
  begin
    Sleep(100);
    Tries := Tries + 1;
  end;
  Tries := 0;
  while not (Unlocked('earwig.exe') and Unlocked('earwigw.exe')) and (Tries < 100) do
  begin
    Sleep(100);
    Tries := Tries + 1;
  end;
  { Still there after 10 s: end it, rather than leave the files behind. }
  if Tries = 100 then
  begin
    Exec('taskkill.exe', '/f /im earwigw.exe /im earwig.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
    Sleep(1000);
  end;
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
