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
  Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\earwig.exe"; Parameters: "quit"; Flags: runhidden; RunOnceId: "QuitEarwig"
