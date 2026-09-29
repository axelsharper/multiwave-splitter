; Inno Setup script for Multiwave Splitter.
; Build: iscc /DAppVersion=0.1.0 installer\windows.iss   (after cargo build --release)
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{8C1F4E1A-5B7D-4C2E-9A61-3E0F2B7D9A11}
AppName=Multiwave Splitter
AppVersion={#AppVersion}
AppPublisher=axelsharper
AppPublisherURL=https://github.com/axelsharper/multiwave-splitter
AppSupportURL=https://github.com/axelsharper/multiwave-splitter/issues
DefaultDirName={autopf}\Multiwave Splitter
DisableProgramGroupPage=yes
; Per-user install by default: no administrator rights needed.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\dist
OutputBaseFilename=MultiwaveSplitter-Windows-Setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\MultiwaveSplitter.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\multiwave-splitter.exe"; DestDir: "{app}"; DestName: "MultiwaveSplitter.exe"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Multiwave Splitter"; Filename: "{app}\MultiwaveSplitter.exe"
Name: "{autodesktop}\Multiwave Splitter"; Filename: "{app}\MultiwaveSplitter.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\MultiwaveSplitter.exe"; Description: "{cm:LaunchProgram,Multiwave Splitter}"; Flags: nowait postinstall skipifsilent
