; Inno Setup script for CodeScope.
; Generates CodeScope-vX.Y.Z-setup.exe via:
;   iscc /DMyAppVersion=X.Y.Z installer\CodeScope.iss
;
; SourceDir=.. so the [Files] entry resolves relative to the repo root
; (CI stages bundle contents into dist\bundle\ before running iscc).
;
; PrivilegesRequired=lowest + DefaultDirName under {localappdata} keeps
; the install per-user (no UAC) so self_update can rewrite the running
; exe without admin.

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0-dev"
#endif

[Setup]
AppId={{B8C7A4D2-3F1E-4A9B-9C5E-7D8F2E1C4A6B}
AppName=CodeScope
AppVersion={#MyAppVersion}
AppVerName=CodeScope v{#MyAppVersion}
AppPublisher=maui1911
AppPublisherURL=https://github.com/maui1911/CodeScope
AppSupportURL=https://github.com/maui1911/CodeScope/issues
AppUpdatesURL=https://github.com/maui1911/CodeScope/releases
DefaultDirName={localappdata}\Programs\CodeScope
DefaultGroupName=CodeScope
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=dist
OutputBaseFilename=CodeScope-v{#MyAppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64
ArchitecturesAllowed=x64
UninstallDisplayIcon={app}\CodeScope.exe
UninstallDisplayName=CodeScope
SourceDir=..

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop icon"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "dist\bundle\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\CodeScope"; Filename: "{app}\CodeScope.exe"
Name: "{group}\Uninstall CodeScope"; Filename: "{uninstallexe}"
Name: "{userdesktop}\CodeScope"; Filename: "{app}\CodeScope.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\CodeScope.exe"; Description: "Launch CodeScope"; Flags: nowait postinstall skipifsilent
