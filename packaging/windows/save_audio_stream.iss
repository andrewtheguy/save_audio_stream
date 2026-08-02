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
// Append to PATH only when it is not already there — an installer that appends
// unconditionally grows PATH by one entry per reinstall.
function NeedsAddPath(Dir: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_LOCAL_MACHINE,
       'SYSTEM\CurrentControlSet\Control\Session Manager\Environment',
       'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;
