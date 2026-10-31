; Windows installer for ferrotype (Inno Setup 6). Built by .github/workflows/release.yml:
;   iscc /DAppVersion=0.2.0 /DSourceExe=..\target\release\ferrotype.exe packaging\ferrotype.iss
; Installs per user by default (no admin prompt); choose "all users" on the first page to install for everyone.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\ferrotype.exe"
#endif

[Setup]
AppId={{6F1E0C52-3B8A-4E57-9C1D-7A2F4B9E8D10}
AppName=ferrotype
AppVersion={#AppVersion}
AppPublisher=ferrotype
AppPublisherURL=https://github.com/hazyyear/Ferrotype
DefaultDirName={autopf}\ferrotype
DefaultGroupName=ferrotype
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=ferrotype-{#AppVersion}-windows-setup
SetupIconFile=..\assets\brand\ferrotype.ico
UninstallDisplayIcon={app}\ferrotype.exe
LicenseFile=..\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "openwith"; Description: "Add ferrotype to ""Open with"" for photos and RAW files"; GroupDescription: "Files:"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion isreadme

[Icons]
Name: "{autoprograms}\ferrotype"; Filename: "{app}\ferrotype.exe"
Name: "{autodesktop}\ferrotype"; Filename: "{app}\ferrotype.exe"; Tasks: desktopicon

[Registry]
; "Open with": registered as an application that can open these types. It never takes over the default program.
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "ferrotype"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\shell\open\command"; ValueType: string; ValueData: """{app}\ferrotype.exe"" ""%1"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\DefaultIcon"; ValueType: string; ValueData: "{app}\ferrotype.exe,0"; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".jpg"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jpg\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".jpeg"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jpeg\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".png"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.png\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".tif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.tif\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".tiff"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.tiff\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".webp"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.webp\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".heic"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.heic\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".heif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.heif\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".avif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.avif\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".jxl"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jxl\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".exr"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.exr\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".dng"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.dng\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".nef"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.nef\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".nrw"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.nrw\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".cr2"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.cr2\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".cr3"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.cr3\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".crw"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.crw\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".arw"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.arw\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".srf"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.srf\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".sr2"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.sr2\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".raf"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.raf\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".orf"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.orf\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".rw2"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.rw2\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".pef"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.pef\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".srw"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.srw\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".x3f"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.x3f\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".3fr"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.3fr\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".iiq"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.iiq\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".erf"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.erf\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".mef"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.mef\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".mos"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.mos\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".kdc"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.kdc\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".dcr"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.dcr\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\ferrotype.exe\SupportedTypes"; ValueType: string; ValueName: ".rwl"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.rwl\OpenWithList\ferrotype.exe"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith

[Run]
Filename: "{app}\ferrotype.exe"; Description: "{cm:LaunchProgram,ferrotype}"; Flags: nowait postinstall skipifsilent
