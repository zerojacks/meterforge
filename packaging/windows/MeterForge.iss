#define AppVersion "0.1.0"
#define SourceDir "..\..\target\release"
#define OutputDir "..\..\dist"

[Setup]
AppId={{8B8B7F7E-5E3D-4A31-9A3B-2FBB2E6F4F01}
AppName=MeterForge
AppVersion={#AppVersion}
AppPublisher=MeterForge
DefaultDirName={localappdata}\Programs\MeterForge
DefaultGroupName=MeterForge
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#OutputDir}
OutputBaseFilename=MeterForge-Setup-{#AppVersion}
Compression=lzma
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\MeterForge.exe

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加快捷方式："; Flags: unchecked

[Files]
Source: "{#SourceDir}\MeterForge.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\MeterForge"; Filename: "{app}\MeterForge.exe"
Name: "{autodesktop}\MeterForge"; Filename: "{app}\MeterForge.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\\MeterForge.exe"; Description: "启动 MeterForge"; Flags: postinstall nowait skipifsilent