#ifndef AppVersion
  #error AppVersion must be supplied by scripts/package.py
#endif

[Setup]
AppId={{29AA6F93-C24A-4A0C-A631-1645D7460F6B}
AppName=Benga
AppVersion={#AppVersion}
AppPublisher=zezic
AppPublisherURL=https://github.com/zezic/benga
AppSupportURL=https://github.com/zezic/benga/issues
AppUpdatesURL=https://benga.zezic.ru
DefaultDirName={localappdata}\Programs\Benga
DefaultGroupName=Benga
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
SetupIconFile={#StageDir}\benga.ico
UninstallDisplayIcon={app}\benga.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
LicenseFile={#StageDir}\LICENSE

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Excludes: ".benga-portable"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Benga"; Filename: "{app}\benga.exe"
Name: "{autodesktop}\Benga"; Filename: "{app}\benga.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\benga.exe"; Description: "Open Benga"; Flags: nowait postinstall skipifsilent
