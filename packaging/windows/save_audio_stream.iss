; Inno Setup script for the save_audio_stream Windows installer.
;
; Built in CI by the `windows` job in .github/workflows/release.yml. ISCC.exe is
; preinstalled on the windows-latest runner image, so there is no toolchain step:
;
;   ISCC.exe /DAppVersion=0.2.15 packaging\windows\save_audio_stream.iss
;
; Requires target\release\save_audio_stream.exe and frontend\dist to exist —
; build both before running this.
;
; Layout, and why it is two trees: Program Files is not user-writable, so
; configuration and recordings cannot live beside the executable the way they do
; under /opt on Linux.
;
;   C:\Program Files\save_audio_stream\      replaced wholesale on upgrade
;     bin\save_audio_stream.exe
;     share\save_audio_stream\web\           served from disk by the binary
;     share\doc\save_audio_stream\*.example
;
;   C:\ProgramData\save_audio_stream\        survives upgrade AND uninstall
;     etc\{record,receiver,credentials}.toml
;     data\recordings\

#define AppName "save_audio_stream"

; Passed by CI. No default: a version-less installer would overwrite the
; Add/Remove Programs entry with a blank one.
#ifndef AppVersion
  #error AppVersion must be defined (ISCC /DAppVersion=x.y.z)
#endif

[Setup]
; Fixed GUID, generated once and never changed: it is what makes the next
; release recognise this install as its own and upgrade it in place, rather than
; landing a second copy beside it.
AppId={{2AC8E742-0CDD-4B4B-AF3C-D7BCC127BF81}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=andrewtheguy
AppPublisherURL=https://github.com/andrewtheguy/save_audio_stream
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
; Per-machine: Program Files, the HKLM PATH entry and the ProgramData ACLs all
; require it.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Broadcasts WM_SETTINGCHANGE so a newly opened shell sees the PATH edit without
; a sign-out.
ChangesEnvironment=yes
UninstallDisplayIcon={app}\bin\{#AppName}.exe
UninstallDisplayName={#AppName} {#AppVersion}
OutputDir=..\..\dist
OutputBaseFilename={#AppName}-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Dirs]
; ProgramData's inherited ACL lets any user create files but not modify ones
; another account created — not enough for a recorder whose SQLite files may be
; written by a different account than the one that installed it. So both trees
; get an explicit grant, and deliberately opposite ones:
;
;   etc\  — read only. It holds credentials.toml; editing configuration is an
;           administrative act.
;   data\ — modify. Recordings, their -wal/-shm siblings and their .lock files
;           are written by whatever account runs the recorder.
Name: "{commonappdata}\{#AppName}"
Name: "{commonappdata}\{#AppName}\etc";             Permissions: users-readexec
Name: "{commonappdata}\{#AppName}\data";            Permissions: users-modify
Name: "{commonappdata}\{#AppName}\data\recordings"; Permissions: users-modify

[Files]
; Paths are relative to this .iss file (packaging\windows\), hence the ..\..
Source: "..\..\target\release\{#AppName}.exe"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "..\..\frontend\dist\*"; DestDir: "{app}\share\{#AppName}\web"; \
        Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\etc\*.toml.example"; DestDir: "{app}\share\doc\{#AppName}"; Flags: ignoreversion

; Seed each config once — the Windows equivalent of seed_config() in
; packaging/install.sh. `onlyifdoesntexist` is the upgrade safety: an existing
; file is the operator's and is left alone. `uninsneveruninstall` is the
; uninstall safety: configuration and recordings outlive the program, exactly as
; <prefix>/etc and <prefix>/data do on Linux.
Source: "..\etc\record.toml.example"; DestDir: "{commonappdata}\{#AppName}\etc"; \
        DestName: "record.toml"; Flags: onlyifdoesntexist uninsneveruninstall
Source: "..\etc\receiver.toml.example"; DestDir: "{commonappdata}\{#AppName}\etc"; \
        DestName: "receiver.toml"; Flags: onlyifdoesntexist uninsneveruninstall
Source: "..\etc\credentials.toml.example"; DestDir: "{commonappdata}\{#AppName}\etc"; \
        DestName: "credentials.toml"; Flags: onlyifdoesntexist uninsneveruninstall

[Tasks]
Name: "addtopath"; Description: "Add {#AppName} to the system PATH"; \
      GroupDescription: "Command line:"

[Registry]
Root: HKLM; Subkey: "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"; \
      ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}\bin"; \
      Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}\bin'))

; No [Icons]: this is a command-line program with no window to launch, and it
; registers no service — the binary has no Service Control Manager dispatcher,
; so a service created with sc.exe would be killed at startup for failing to
; respond. Run it from a command prompt.
;
; No [UninstallDelete] under {commonappdata} either: uninstalling must not take
; the recordings or the edited configuration with it.

[Code]
const
  EnvironmentKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  // Deliberately not named FILE_ATTRIBUTE_REPARSE_POINT / INVALID_FILE_ATTRIBUTES:
  // Inno Setup predefines those in Pascal Script, and redeclaring one is a
  // "Duplicate identifier" compile error rather than a shadow. Using the Win32
  // names would work only for as long as this file does not also need a
  // constant Inno has not defined, so both carry local names instead.
  ReparsePointAttr = $00000400;
  InvalidFileAttrs = $FFFFFFFF;

function ApiGetFileAttributes(lpFileName: string): Cardinal;
  external 'GetFileAttributesW@kernel32.dll stdcall';

// True if the path exists and is a junction, symlink or other reparse point.
//
// This matters because ProgramData's default ACL lets any authenticated user
// *create* subdirectories under it. A standard user can therefore pre-create
// C:\ProgramData\save_audio_stream (or its etc\ subdirectory) as a junction
// pointing somewhere they control, before this installer ever runs. The
// installer runs elevated, so it would then apply the [Dirs] ACLs to, and seed
// credentials.toml into, an attacker-chosen location — or, with
// `onlyifdoesntexist`, silently adopt an attacker-planted config as though it
// were the operator's own.
function IsReparsePoint(TargetPath: string): Boolean;
var
  Attrs: Cardinal;
begin
  Result := False;
  // Check existence through Inno first. The "failed" sentinel is $FFFFFFFF --
  // every bit set, the reparse bit included -- so a bare bit test would call
  // every *absent* path a reparse point and refuse every fresh install. Testing
  // existence first means the sentinel comparison below is only ever reached
  // for a path that is really there, and does not have to depend on how a
  // Cardinal and a $FFFFFFFF literal compare in Pascal Script.
  if (not FileExists(TargetPath)) and (not DirExists(TargetPath)) then
    exit;
  Attrs := ApiGetFileAttributes(TargetPath);
  Result := (Attrs <> InvalidFileAttrs) and
            ((Attrs and ReparsePointAttr) <> 0);
end;

// Refuse to install onto a redirected configuration or data tree rather than
// trying to repair one: the safe repair is indistinguishable from deleting
// something the operator set up deliberately (a data directory relocated onto
// another volume is a legitimate junction). Aborting names the path and leaves
// the decision with whoever can tell the two apart.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Base, Blank: string;
  Suspect: array[0..5] of string;
  I: Integer;
begin
  Result := '';
  // Chr(13) + Chr(10) rather than #13#10: the preprocessor runs first and
  // treats a '#' in the first column as a directive, so a wrapped line that
  // happens to begin with a character literal fails the build with "Unknown
  // preprocessor directive". Building the separator once sidesteps the question
  // of where the line breaks fall.
  Blank := Chr(13) + Chr(10) + Chr(13) + Chr(10);
  Base := ExpandConstant('{commonappdata}\{#AppName}');
  Suspect[0] := Base;
  Suspect[1] := Base + '\etc';
  Suspect[2] := Base + '\data';
  Suspect[3] := Base + '\etc\record.toml';
  Suspect[4] := Base + '\etc\receiver.toml';
  Suspect[5] := Base + '\etc\credentials.toml';
  for I := 0 to 5 do
  begin
    if IsReparsePoint(Suspect[I]) then
    begin
      // Every join is an explicit '+': adjacent-literal concatenation
      // ('a' #13#10 'b') is Delphi syntax that Inno's Pascal Script does not
      // accept.
      Result := 'Refusing to install: ' + Suspect[I] +
                ' is a junction, symbolic link or other reparse point.' +
                Blank +
                'Configuration and recordings must live under ' + Base +
                ' itself, not be redirected elsewhere - an elevated installer' +
                ' following a link placed there by a standard user would write' +
                ' credentials to a location that user controls.' +
                Blank +
                'Remove or replace that path with a real directory, then run' +
                ' this installer again.';
      exit;
    end;
  end;
end;

// Append to PATH only when it is not already there — an installer that appends
// unconditionally grows PATH by one entry per reinstall.
function NeedsAddPath(Dir: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_LOCAL_MACHINE, EnvironmentKey, 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;

// Take the PATH entry back out on uninstall.
//
// Inno cannot do this declaratively: the [Registry] entry appends to an
// existing value, and the only automatic counterpart, `uninsdeletevalue`, would
// delete the *entire* system PATH. So the segment is removed by hand, matched
// case-insensitively the same way NeedsAddPath adds it, and only if it is
// actually present — an uninstall must not rewrite PATH when it put nothing
// there (the task is opt-in) or when the value is missing entirely.
procedure RemoveFromPath(Dir: string);
var
  OrigPath, Padded: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKEY_LOCAL_MACHINE, EnvironmentKey, 'Path', OrigPath) then
    exit;
  Padded := ';' + OrigPath + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Padded));
  if P = 0 then
    exit;
  // P indexes the leading ';' of the match; drop the directory and the
  // separator that follows it, leaving that leading ';' to join the neighbours.
  Delete(Padded, P + 1, Length(Dir) + 1);
  // Strip the sentinel ';' from each end.
  RegWriteExpandStringValue(HKEY_LOCAL_MACHINE, EnvironmentKey, 'Path',
    Copy(Padded, 2, Length(Padded) - 2));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // usPostUninstall: the files are gone but {app} still expands, which is what
  // the PATH entry was built from. ChangesEnvironment=yes broadcasts the change.
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}\bin'));
end;
