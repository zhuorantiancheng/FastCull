#ifndef Payload
  #error Payload must name the staged Windows distribution
#endif
#ifndef Version
  #define Version "2.7.7"
#endif

[Setup]
AppId={{35C2DD44-734B-4D7B-9997-23CC5FF65101}
AppName=FastCull
AppVersion={#Version}
AppPublisher=FastCull contributors
DefaultDirName={localappdata}\Programs\FastCull
DefaultGroupName=FastCull
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
DisableProgramGroupPage=yes
LicenseFile={#Payload}\LICENSE
InfoAfterFile={#Payload}\QUICKSTART.txt
#ifndef QA
OutputBaseFilename=FastCull-Setup-{#Version}-windows-x64
#endif
Compression=lzma2
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
SetupLogging=yes
UninstallDisplayIcon={app}\FastCull.exe
#ifdef QA
Uninstallable=no
CreateUninstallRegKey=no
OutputBaseFilename=FastCull-QA-{#Version}
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Types]
Name: "full"; Description: "FastCull + Adobe integrations"
Name: "standalone"; Description: "FastCull standalone only"
Name: "custom"; Description: "Custom"; Flags: iscustom

[Components]
Name: "core"; Description: "FastCull (required)"; Types: full standalone custom; Flags: fixed
Name: "bridge"; Description: "Register / update Adobe Bridge integration"; Types: full
Name: "lightroom"; Description: "Register / update Lightroom Classic integration"; Types: full

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#Payload}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs; Components: core

[Icons]
#ifndef QA
Name: "{group}\FastCull"; Filename: "{app}\FastCull.exe"
Name: "{group}\Adobe integration instructions"; Filename: "{app}\QUICKSTART.txt"
Name: "{group}\Uninstall FastCull"; Filename: "{uninstallexe}"
Name: "{autodesktop}\FastCull"; Filename: "{app}\FastCull.exe"; Tasks: desktopicon
#endif

[Run]
Filename: "{app}\FastCull.exe"; Description: "Launch FastCull"; Flags: nowait postinstall skipifsilent unchecked

[Code]
var RegistrationFailed: Boolean;

function InitializeSetup(): Boolean;
begin
  RegistrationFailed := False;
  Result := True;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var Path: String;
begin
  Result := True;
  if CurPageID = wpSelectDir then begin
    Path := WizardDirValue;
    if (Pos('%', Path) > 0) or (Pos('!', Path) > 0) or (Pos('"', Path) > 0) then begin
      MsgBox('Choose a path without %, ! or quotation marks (Adobe launcher limitation).', mbError, MB_OK);
      Result := False;
    end;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var Args: String; Code: Integer;
begin
  if (CurStep = ssPostInstall) and (WizardIsComponentSelected('bridge') or WizardIsComponentSelected('lightroom')) then begin
    Args := '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}\scripts\register-integrations.ps1') + '"';
    if WizardIsComponentSelected('bridge') then Args := Args + ' -Bridge';
    if WizardIsComponentSelected('lightroom') then Args := Args + ' -Lightroom';
    if not Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'), Args, '', SW_HIDE, ewWaitUntilTerminated, Code) then Code := -1;
    RegistrationFailed := Code <> 0;
    if RegistrationFailed then begin
      Log('Adobe integration registration failed: ' + IntToStr(Code));
      if not WizardSilent then MsgBox('FastCull files were installed, but Adobe integration failed. Close Adobe apps and run Setup again. See QUICKSTART.txt.', mbError, MB_OK);
    end;
  end;
end;

function GetCustomSetupExitCode(): Integer;
begin
  Result := 0;
  if RegistrationFailed then Result := 1;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var Code: Integer; Args: String;
begin
  if CurUninstallStep = usUninstall then begin
    Args := '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}\scripts\register-integrations.ps1') + '" -Mode Uninstall';
    if not Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'), Args, '', SW_HIDE, ewWaitUntilTerminated, Code) then Code := -1;
    if Code <> 0 then begin
      MsgBox('Adobe cleanup failed. FastCull will remain installed. Close Adobe apps and try again.', mbError, MB_OK);
      Abort;
    end;
  end;
end;
