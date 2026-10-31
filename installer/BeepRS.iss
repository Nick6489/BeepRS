; Keep AppId in sync with src/update.rs APP_ID, including the braces.
#ifndef AppVersion
  #error Define AppVersion, for example /DAppVersion=1.0.3
#endif
#ifndef SourceDir
  #error Define SourceDir to the staged install tree (exe, sounds, marker)
#endif
#ifndef OutputDir
  #error Define OutputDir
#endif

[Setup]
AppId={{7B3E9C4A-1F62-4D8E-A5B1-90C2E84F6D17}}
AppName=BeepRS
AppVersion={#AppVersion}
AppPublisher=Nick6489
AppPublisherURL=https://github.com/Nick6489/BeepRS
DefaultDirName={localappdata}\Programs\BeepRS
DefaultGroupName=BeepRS
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
SetupArchitecture=x64
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=BeepRS-{#AppVersion}
UninstallDisplayIcon={app}\beeprs.exe
CloseApplications=no
RestartApplications=no
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile=..\LICENSE

[Files]
Source: "{#SourceDir}\beeprs.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\sounds\*"; DestDir: "{app}\sounds"; Flags: ignoreversion recursesubdirs
Source: "{#SourceDir}\beeprs-distribution"; DestDir: "{app}"; Flags: ignoreversion
#include "..\..\freshen\inno\FreshenInno.iss"

[Icons]
Name: "{group}\BeepRS"; Filename: "{app}\beeprs.exe"

[Run]
Filename: "{app}\beeprs.exe"; Description: "Play BeepRS"; Flags: nowait postinstall skipifsilent
