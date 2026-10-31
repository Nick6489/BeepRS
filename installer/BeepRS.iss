; Keep AppId in sync with src/update.rs APP_ID, including the braces.
; Only the opening brace is doubled. "{{GUID}" is "{GUID}". A second closing
; brace is stored in the AppId, and Freshen will not find the uninstall key.
#ifndef AppVersion
  #error Define AppVersion, for example /DAppVersion=1.0.5
#endif
#ifndef SourceDir
  #error Define SourceDir to the staged install tree (exe, sounds, marker)
#endif
#ifndef OutputDir
  #error Define OutputDir
#endif

[Setup]
AppId={{7B3E9C4A-1F62-4D8E-A5B1-90C2E84F6D17}
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

; 1.0.3 and 1.0.4 stored a spare closing brace in the AppId, so their uninstall
; key is {GUID}}_is1 and their log is unins000. This script does not match that
; log. Inno writes a new one. Remove the mistyped key, and remove leftover
; unins files that are not the registered uninstaller.
[Code]
const
  UninstallKeyPrefix = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\';

function GoodAppId: String;
begin
  Result := Chr(123) + '7B3E9C4A-1F62-4D8E-A5B1-90C2E84F6D17' + Chr(125);
end;

function UninstallSubkey(const AppId: String): String;
begin
  Result := UninstallKeyPrefix + AppId + '_is1';
end;

procedure RemoveUninstallKey(const Subkey: String);
begin
  if RegKeyExists(HKCU64, Subkey) and RegDeleteKeyIncludingSubkeys(HKCU64, Subkey) then
    Log('Removed HKCU64 ' + Subkey);
  if RegKeyExists(HKCU32, Subkey) and RegDeleteKeyIncludingSubkeys(HKCU32, Subkey) then
    Log('Removed HKCU32 ' + Subkey);
end;

function QueryUninstallString(const Subkey: String; var UninstallString: String): Boolean;
begin
  Result := RegQueryStringValue(HKCU64, Subkey, 'UninstallString', UninstallString);
  if not Result then
    Result := RegQueryStringValue(HKCU32, Subkey, 'UninstallString', UninstallString);
end;

function RegisteredUninstallerName(const UninstallString: String): String;
var
  Value: String;
  Mark: Integer;
begin
  Value := Trim(UninstallString);
  if (Length(Value) >= 2) and (Value[1] = '"') then
  begin
    Delete(Value, 1, 1);
    Mark := Pos('"', Value);
    if Mark > 0 then
      Value := Copy(Value, 1, Mark - 1);
  end
  else
  begin
    Mark := Pos(' ', Value);
    if Mark > 0 then
      Value := Copy(Value, 1, Mark - 1);
  end;
  Result := ExtractFileName(Value);
end;

function UninstallStem(const FileName: String): String;
var
  Extension: String;
begin
  Extension := ExtractFileExt(FileName);
  Result := LowerCase(Copy(FileName, 1, Length(FileName) - Length(Extension)));
end;

<event('CurStepChanged')>
procedure BeepRSStepChanged(CurStep: TSetupStep);
var
  UninstallString, Registered, AppDir, Extension, Stem: String;
  Found: TFindRec;
begin
  if CurStep <> ssPostInstall then
    Exit;
  RemoveUninstallKey(UninstallSubkey(GoodAppId + Chr(125)));
  UninstallString := '';
  if not QueryUninstallString(UninstallSubkey(GoodAppId), UninstallString) then
  begin
    Log('Current uninstall key was not readable. Left uninstall files in place.');
    Exit;
  end;
  Registered := RegisteredUninstallerName(UninstallString);
  if Registered = '' then
  begin
    Log('Could not read the registered uninstaller name. Left uninstall files in place.');
    Exit;
  end;
  Log('Registered uninstaller is ' + Registered);
  AppDir := AddBackslash(ExpandConstant('{app}'));
  if not FindFirst(AppDir + 'unins*', Found) then
    Exit;
  try
    repeat
      Extension := LowerCase(ExtractFileExt(Found.Name));
      if (Extension = '.exe') or (Extension = '.dat') or (Extension = '.msg') then
      begin
        Stem := UninstallStem(Found.Name);
        if Stem <> UninstallStem(Registered) then
        begin
          if DeleteFile(AppDir + Found.Name) then
            Log('Removed leftover ' + Found.Name);
        end;
      end;
    until not FindNext(Found);
  finally
    FindClose(Found);
  end;
end;
