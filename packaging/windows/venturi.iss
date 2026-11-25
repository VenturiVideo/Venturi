; Compiled by scripts\build-windows.ps1, which passes /DAppVersion and /DStage.
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef Stage
  #error Stage is required
#endif

[Setup]
; Never change it: upgrades and the uninstaller find the install through it.
AppId={{33B5DEE5-B612-4456-A712-2E61E457C3EC}
AppName=Venturi
AppVersion={#AppVersion}
AppPublisher=Venturi
DefaultDirName={autopf}\Venturi
DefaultGroupName=Venturi
DisableProgramGroupPage=yes
; Per-user by default, so no admin rights are needed; the dialog offers all users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
ChangesAssociations=yes
LicenseFile={#Stage}\LICENSE.txt
SetupIconFile=..\..\media\icons\windows\venturi-video.ico
UninstallDisplayIcon={app}\venturi.exe
OutputDir=..\..\target\windows
OutputBaseFilename=Venturi-{#AppVersion}-x86_64-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "italian"; MessagesFile: "compiler:Languages\Italian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Stage}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{group}\Venturi"; Filename: "{app}\venturi.exe"
Name: "{group}\{cm:UninstallProgram,Venturi}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Venturi"; Filename: "{app}\venturi.exe"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "Software\Classes\.vvproj"; ValueType: string; ValueName: ""; ValueData: "Venturi.Project"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Venturi.Project"; ValueType: string; ValueName: ""; ValueData: "Venturi project"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Venturi.Project\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\venturi.exe,0"
Root: HKA; Subkey: "Software\Classes\Venturi.Project\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\venturi.exe"" ""%1"""

[Run]
Filename: "{app}\venturi.exe"; Description: "{cm:LaunchProgram,Venturi}"; Flags: nowait postinstall skipifsilent
