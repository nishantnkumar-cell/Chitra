; Inno Setup script for the Chitra Windows installer.
; CI builds it with:  ISCC /DAppVersion=x.y.z installer\chitra.iss
; after `cargo build --release`. The installer lands in dist\.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BinDir
  #define BinDir "..\target\release"
#endif

[Setup]
AppId={{6F1C2B7E-3D4A-4E59-9B1F-8C2A7D5E4C31}
AppName=Chitra
AppVersion={#AppVersion}
AppVerName=Chitra {#AppVersion}
AppPublisher=Chitra
AppPublisherURL=https://github.com/nishantnkumar-cell/Chitra
AppSupportURL=https://github.com/nishantnkumar-cell/Chitra/issues
AppUpdatesURL=https://github.com/nishantnkumar-cell/Chitra/releases
VersionInfoVersion={#AppVersion}
VersionInfoDescription=Chitra photo viewer and converter setup
; Installs for the current user without asking for admin rights; the
; first page offers "install for all users" instead.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\Chitra
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=NOTICE.txt
SetupIconFile=..\assets\chitra.ico
UninstallDisplayIcon={app}\chitra.exe
UninstallDisplayName=Chitra
OutputDir=..\dist
OutputBaseFilename=Chitra-Setup-{#AppVersion}-x64
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "openwith"; Description: "Add Chitra to ""Open with"" for photos, and ""Browse with Chitra"" for folders"; GroupDescription: "Explorer:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\chitra.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\chitra-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "NOTICE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "LICENSE-AGPL-3.0.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Chitra"; Filename: "{app}\chitra.exe"
Name: "{autodesktop}\Chitra"; Filename: "{app}\chitra.exe"; Tasks: desktopicon

[Registry]
; A ProgId plus OpenWithProgids entries put Chitra in "Open with" without
; taking over the user's default photo app (Windows lets only the user
; choose that).
Root: HKA; Subkey: "Software\Classes\Chitra.Image"; ValueType: string; ValueData: "Photo"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Chitra.Image\DefaultIcon"; ValueType: string; ValueData: "{app}\chitra.exe,0"; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Chitra.Image\shell\open\command"; ValueType: string; ValueData: """{app}\chitra.exe"" ""%1"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "Chitra"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\shell\open\command"; ValueType: string; ValueData: """{app}\chitra.exe"" ""%1"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jpg\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".jpg"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jpeg\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".jpeg"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jpe\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".jpe"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jfif\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".jfif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jxl\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".jxl"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.png\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".png"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.apng\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".apng"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.gif\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".gif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.bmp\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".bmp"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.dib\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".dib"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.webp\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".webp"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.avif\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".avif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.heic\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".heic"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.heif\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".heif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.hif\OpenWithProgids"; ValueType: string; ValueName: "Chitra.Image"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\chitra.exe\SupportedTypes"; ValueType: string; ValueName: ".hif"; ValueData: ""; Tasks: openwith
; "Browse with Chitra" on folders opens the folder overview.
Root: HKA; Subkey: "Software\Classes\Directory\shell\Chitra"; ValueType: string; ValueData: "Browse with Chitra"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\shell\Chitra"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\chitra.exe,0"; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\shell\Chitra\command"; ValueType: string; ValueData: """{app}\chitra.exe"" ""%1"""; Tasks: openwith

[Run]
Filename: "{app}\chitra.exe"; Description: "{cm:LaunchProgram,Chitra}"; Flags: nowait postinstall skipifsilent
