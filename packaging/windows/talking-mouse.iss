; Inno Setup script for the Windows installer.
; Build (from the repo root, after `cargo build --release` and staging files):
;   iscc /DAppVersion=0.1.0 /DSourceDir=C:\path\to\dist\windows /DOutputDir=C:\path\to\dist packaging\windows\talking-mouse.iss
; SourceDir must contain talking-mouse.exe, talking-mouse-editor.exe and an
; ffmpeg\ folder with ffmpeg.exe and ffprobe.exe.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\windows"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
AppId={{6C1C2E7A-5D0B-4F2B-9E7C-2B7A1F4D9C31}
AppName=Talking Mouse
AppVersion={#AppVersion}
AppVerName=Talking Mouse {#AppVersion}
AppPublisher=BitLess246
AppPublisherURL=https://github.com/BitLess246/talking-mouse
AppSupportURL=https://github.com/BitLess246/talking-mouse/issues
DefaultDirName={autopf}\Talking Mouse
DefaultGroupName=Talking Mouse
DisableProgramGroupPage=yes
; Installs per-user without admin rights; the user can opt into all-users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir={#OutputDir}
OutputBaseFilename=TalkingMouse-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\assets\icon\icon.ico
UninstallDisplayIcon={app}\talking-mouse-editor.exe
UninstallDisplayName=Talking Mouse
LicenseFile=..\..\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesEnvironment=yes
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "Add the talking-mouse command-line tool to PATH"; GroupDescription: "Command line:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\talking-mouse-editor.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\talking-mouse.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\ffmpeg\*"; DestDir: "{app}\ffmpeg"; Flags: ignoreversion recursesubdirs
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\..\README.md"; DestDir: "{app}"

[Icons]
Name: "{autoprograms}\Talking Mouse"; Filename: "{app}\talking-mouse-editor.exe"; Comment: "Record your screen and pin captions to the mouse"
Name: "{autodesktop}\Talking Mouse"; Filename: "{app}\talking-mouse-editor.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\talking-mouse-editor.exe"; Description: "{cm:LaunchProgram,Talking Mouse}"; Flags: nowait postinstall skipifsilent

[Code]
// Per-user installs edit the user's PATH; all-users installs the system one.
function EnvRoot: Integer;
begin
  if IsAdminInstallMode then Result := HKEY_LOCAL_MACHINE else Result := HKEY_CURRENT_USER;
end;

function EnvKey: String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

procedure AddToPath(Dir: String);
var
  Paths: String;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey, 'Path', Paths) then Paths := '';
  if Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0 then Exit;
  if (Paths <> '') and (Copy(Paths, Length(Paths), 1) <> ';') then Paths := Paths + ';';
  RegWriteExpandStringValue(EnvRoot, EnvKey, 'Path', Paths + Dir);
end;

procedure RemoveFromPath(Dir: String);
var
  Paths, Padded: String;
  P: Integer;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey, 'Path', Paths) then Exit;
  Padded := ';' + Paths + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Padded));
  if P = 0 then Exit;
  // Drop ";Dir", keeping the separator that followed it, then the padding.
  Delete(Padded, P, Length(Dir) + 1);
  RegWriteExpandStringValue(EnvRoot, EnvKey, 'Path', Copy(Padded, 2, Length(Padded) - 2));
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
