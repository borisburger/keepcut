; Inno Setup 7 script for Keepcut. Built by package.ps1, which stages the files and passes:
;   /DAppVersion=0.1.0  /DSourceDir=<staged folder>  /DOutputDir=<dist folder>
; Installs per user by default (no admin rights); the first page offers "all users".

#ifndef AppVersion
  #error AppVersion not defined (run package.ps1)
#endif

[Setup]
; Never change the AppId: it identifies the installation for upgrades and the uninstaller.
AppId={{0ADADBDA-4000-471B-B94D-904046EF1636}
AppName=Keepcut
AppVersion={#AppVersion}
AppVerName=Keepcut {#AppVersion}
AppPublisher=Boris Burger
AppPublisherURL=https://github.com/borisburger/keepcut
AppSupportURL=https://github.com/borisburger/keepcut/issues
DefaultDirName={autopf}\Keepcut
DefaultGroupName=Keepcut
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern dynamic
SetupIconFile=..\icons\keepcut.ico
UninstallDisplayIcon={app}\keepcut.exe
UninstallDisplayName=Keepcut
ChangesAssociations=yes
CloseApplications=yes
Compression=lzma2/ultra64
SolidCompression=yes
; ffmpeg.exe and ffprobe.exe (~110 MB each) are statically linked and nearly identical; a
; dictionary larger than one of them lets LZMA store the second one almost for free
; (98 -> ~75 MB). Setup needs about this much RAM to unpack.
LZMADictionarySize=262144
LZMAUseSeparateProcess=yes
OutputDir={#OutputDir}
OutputBaseFilename=Keepcut-{#AppVersion}-windows-x64-setup
VersionInfoVersion={#AppVersion}
VersionInfoProductName=Keepcut

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "assoc"; Description: "Open .keepcut project files with Keepcut"; GroupDescription: "File association:"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Keepcut"; Filename: "{app}\keepcut.exe"
Name: "{autodesktop}\Keepcut"; Filename: "{app}\keepcut.exe"; Tasks: desktopicon

[Registry]
; .keepcut projects (HKA = HKCU for a per-user install, HKLM for all users)
Root: HKA; Subkey: "Software\Classes\.keepcut"; ValueType: string; ValueName: ""; ValueData: "Keepcut.Project"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.keepcut\OpenWithProgids"; ValueType: string; ValueName: "Keepcut.Project"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\Keepcut.Project"; ValueType: string; ValueName: ""; ValueData: "Keepcut project"; Flags: uninsdeletekey; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\Keepcut.Project\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\keepcut.exe,0"; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\Keepcut.Project\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\keepcut.exe"" ""%1"""; Tasks: assoc
; "Open with" for video files
Root: HKA; Subkey: "Software\Classes\Applications\keepcut.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "Keepcut"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\keepcut.exe\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\keepcut.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\Applications\keepcut.exe\SupportedTypes"; ValueType: string; ValueName: ".mp4"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\Applications\keepcut.exe\SupportedTypes"; ValueType: string; ValueName: ".mov"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\Applications\keepcut.exe\SupportedTypes"; ValueType: string; ValueName: ".mkv"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\Applications\keepcut.exe\SupportedTypes"; ValueType: string; ValueName: ".keepcut"; ValueData: ""

[Run]
Filename: "{app}\keepcut.exe"; Description: "{cm:LaunchProgram,Keepcut}"; Flags: nowait postinstall skipifsilent
