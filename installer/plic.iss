; Inno Setup script for the plic Windows installer.
;
; Built by .github/workflows/release.yml. To build it locally:
;   cargo build --release
;   iscc /DAppVersion=0.8.0 installer\plic.iss
; The version comes from Cargo.toml; the release workflow passes it in.

#ifndef AppVersion
  #error AppVersion must be defined, e.g. iscc /DAppVersion=0.8.0 installer\plic.iss
#endif

#define AppName "plic"
#define AppExe "plic.exe"

[Setup]
AppId={{6F1C2B7A-3D4E-4A8B-9C0D-5E6F7A8B9C01}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=enekocm7
AppPublisherURL=https://github.com/enekocm7/plic
AppSupportURL=https://github.com/enekocm7/plic/issues
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Per-user install by default; no admin prompt unless the user asks for it.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\assets\plic.ico
UninstallDisplayIcon={app}\{#AppExe}
OutputDir=..\target\dist
OutputBaseFilename=plic-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
