; Per-user installer for earwig: no administrator rights, installs to
; %LOCALAPPDATA%\Programs\earwig, a Start menu entry, optional start at
; sign-in. Build with `iscc /DVersion=<x.y.z> /DStage=<dir> earwig.iss`
; (package.ps1 does), where <dir> holds earwig.exe and earwigw.exe.

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

[Tasks]
Name: autostart; Description: "Start earwig when I sign in"

[Files]
Source: "{#Stage}\earwig.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\earwigw.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"
Source: "..\..\assets\fonts\LICENSE-IBM-Plex-Sans.txt"; DestDir: "{app}"
Source: "..\..\README.md"; DestDir: "{app}"

[Icons]
Name: "{userprograms}\earwig"; Filename: "{app}\earwigw.exe"; Parameters: "settings"

[Registry]
; The same entry the settings window's "Start at sign-in" switch writes.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; \
  ValueName: "earwig"; ValueData: """{app}\earwigw.exe"""; Tasks: autostart; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "earwig"; \
  Flags: dontcreatekey uninsdeletevalue

[Run]
Filename: "{app}\earwigw.exe"; Parameters: "settings"; Description: "Start earwig"; \
  Flags: nowait postinstall skipifsilent; Check: not WasRunning
; An upgrade brings back the earwig it stopped, silent ones (winget) too.
Filename: "{app}\earwigw.exe"; Flags: nowait; Check: WasRunning

[UninstallRun]
Filename: "{app}\earwig.exe"; Parameters: "quit"; Flags: runhidden; RunOnceId: "QuitEarwig"

[Code]
var
  Running: Boolean;

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
  if FileExists(ExpandConstant('{app}\earwig.exe')) and Earwig('quit') then
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
