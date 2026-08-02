# Packaging

How `save_audio_stream` is built into a release artifact and laid down on disk.

| File | What it does |
| --- | --- |
| `build-tarball.sh` | Builds the frontend + release binary and assembles `dist/save_audio_stream-<version>-<os>-<arch>.tar.gz` |
| `install.sh` | Ships **inside** the tarball. Lays the tree down under `<prefix>`, seeds config, flips `current` |
| `uninstall.sh` | Ships inside the tarball. Removes versions; keeps config and recordings unless `--purge` |
| `Dockerfile` | Runtime image built from an *extracted tarball* — nothing is compiled |
| `etc/*.toml.example` | Config templates, shipped to `share/doc/save_audio_stream/` and seeded into `<prefix>/etc` |
| `release_version.py` | The one definition of a releasable version — checked against Cargo's grammar *and* the OCI image-tag grammar |
| `windows/save_audio_stream.iss` | Inno Setup script for the Windows installer |
| `../install.sh` | Network installer: downloads a release, verifies its SHA-256, runs the bundled `install.sh` |

Images and installers are built **only in CI** (`.github/workflows/release.yml`).
There is no local `docker build` path by design: the image must contain the same
bytes that were attached to the release.

## Installed layout (Linux/macOS)

```
/opt/save_audio_stream/
├── etc/                              # 0700 — seeded once, never rolled with versions
│   ├── record.toml                   # 0600
│   ├── receiver.toml                 # 0600
│   └── credentials.toml              # 0600
├── data/                             # 0700 — sibling of versions/, never pruned
│   └── recordings/                   #   *.sqlite (+ -wal/-shm), <name>.lock, audio
├── versions/
│   ├── 0.2.14/
│   │   ├── VERSION
│   │   ├── bin/save_audio_stream
│   │   └── share/
│   │       ├── save_audio_stream/web/           # the frontend, served from disk
│   │       └── doc/save_audio_stream/*.example
│   └── 0.2.15/
├── current -> versions/0.2.15        # flipped with an atomic rename(2)
└── .install.lock                     # mkdir-based mutex, held for one install

/usr/local/bin/save_audio_stream -> /opt/save_audio_stream/current/bin/save_audio_stream
```

The binary finds all of this from its own location — `current_exe()` is
canonicalized, so both the launcher symlink and `current` resolve to the real
`versions/<v>/bin` path, and `share/`, `etc/` and `data/` are derived from
there. See `src/paths.rs`. Nothing is compiled in, so a tarball is relocatable:
`PREFIX=/srv/sas BINDIR=~/bin ./install.sh` works with no rebuild.

### Running the tarball without installing it

`install.sh` is a convenience, not a requirement — the tarball is a working tree
on its own. Extracted anywhere and run as `<extracted>/bin/save_audio_stream`,
the binary still finds `share/save_audio_stream/web` beside itself, because that
lookup is `<exe>/../share/...` and holds in every layout.

Config and data do *not* resolve to the extracted directory, and that is the
point of the shape check in `installed_prefix()`: it requires the component
above the version root to be literally named `versions` before it will claim a
prefix. An extracted tarball does not match, so it falls back to the per-user
directories instead of inventing `<extracted>/../etc` out of whatever the user
happened to untar into. `SAVE_AUDIO_STREAM_CONFIG_DIR` and
`SAVE_AUDIO_STREAM_DATA_DIR` make it fully self-contained if that is what is
wanted.

Local build:

```sh
cd frontend && bun install --frozen-lockfile && cd ..
bash packaging/build-tarball.sh
```

## Where data lives, and why it is outside `versions/`

`<prefix>/data` is a **sibling** of `versions/`, and that placement is the whole
upgrade story:

- **Pruning cannot reach it.** `install.sh` prunes with a loop over
  `"$prefix"/versions/*/`, keeping the new version and the immediately previous
  one. A directory outside `versions/` is not matched by that glob. Data under
  `versions/<v>/` would be deleted on the second upgrade after it was written —
  silently, with recordings in it.
- **Rollback does not move it.** Rollback re-points one symlink. A path derived
  from the *version root* would follow `current` and roll back with the binary,
  abandoning everything recorded since the upgrade. `data/` is derived from the
  *prefix*, which does not change.
- **The lock files need one path.** `src/record.rs` takes an exclusive lock on
  `<output_dir>/<name>.lock` for the process lifetime. An old binary still
  running and a new one starting must contend on the *same* file — otherwise
  they would each take their own lock and both record into the same database.

The same reasoning applies to `etc/`.

## Rollback and the SQLite schema version

Rollback is a single symlink flip:

```sh
ln -sfn versions/0.2.14 /opt/save_audio_stream/current
```

**It restores the binary, not the data.** `src/constants.rs` pins
`EXPECTED_DB_VERSION`, and this project keeps no SQLite migration path. So if a
release bumps that constant:

- the new binary will not open databases written by the old one, and
- after the upgrade has run, the old binary will not open the new ones either.

Rolling back across such a release leaves the previous binary pointed at data it
refuses; the only recovery is to remove `<prefix>/data/recordings/*.sqlite*`,
which loses the recordings made under the new version.

Practically: **a release that bumps `EXPECTED_DB_VERSION` is one-way.** Its
release notes must say so, and `<prefix>/data` should be backed up before
installing it. `install.sh` prints this warning whenever it replaces a different
version.

PostgreSQL (receiver mode) is unaffected — it is an external server addressed by
`[database].url`, and it is the one store this project does treat as long-lived.

## Windows

Program Files is not user-writable, so the tree splits in two. The executable
still sits at `<root>\bin\`, which is what lets one `install_root()` cover both
platforms.

```
C:\Program Files\save_audio_stream\      admin-writable only; replaced on upgrade
├── bin\save_audio_stream.exe
├── share\save_audio_stream\web\
├── share\doc\save_audio_stream\*.example
└── unins000.exe

C:\ProgramData\save_audio_stream\        writable; survives upgrade and uninstall
├── etc\{record,receiver,credentials}.toml
└── data\recordings\
```

There is no `versions/` on Windows: the installer is the version manager. A
stable `AppId` makes the next installer an in-place upgrade rather than a second
copy, and Add/Remove Programs owns the uninstall. Rolling back means running the
previous `setup.exe` — with the same schema caveat as above. Config and
recordings survive both, because they are in ProgramData and the uninstaller
never touches it.

The installer adds `<install dir>\bin` to the system PATH (an opt-in task) and
does nothing else: `save_audio_stream` is run from a command prompt. No service
is registered — the binary has no Service Control Manager dispatcher, so a
service created with `sc.exe` would be killed at startup for not responding.

## Releasing

`.github/workflows/release.yml`, `workflow_dispatch` only:

```
prepare ──> frontend ──┬──> build (linux x86_64, linux arm64, macos arm64)
    │                  └──> windows (Inno Setup installer)
    └──────────────────────┴──> release ──> docker (amd64, arm64) ──> docker-manifest
```

`prepare` reads the version from `Cargo.toml` with the same TOML parse
`build-tarball.sh` uses, refuses to reuse an existing tag, and opens a **draft**
release — assets attach to the draft and the tag is created only when it is
published, so a failed build never leaves a half-populated release behind.
`frontend` builds the bundle once and every downstream job consumes that
artifact, so the tarballs, the Windows installer and both images ship identical
frontend bytes. `build` does not cross-compile — each tarball is produced on a
runner of its own platform, which is why macOS ships arm64 only (see the matrix
comment for why Intel is excluded). The `docker` job runs after the release is published and builds
from the published tarball, smoke-testing each image before it is pushed.

To cut a release: bump the version (`uv run scripts/bump_version.py <version>`),
commit, then run the workflow.

### What counts as a releasable version

`release_version.py` is the single answer, shared by `bump_version.py`,
`build-tarball.sh` and the workflow's `prepare` job, so the three cannot
disagree. It validates against both grammars the version has to satisfy:

- **Cargo's** — SemVer 2.0.0, which Cargo enforces strictly. `01.2.3`, `1.2`,
  `1.2.3-`, `1.2.3-01` and `1.2.3-RC_1` are all rejected by `cargo` itself, so
  they are rejected here too rather than at `cargo build`, three jobs later.
- **The OCI tag grammar** — applied to the tag the release actually builds
  (`v<version>-amd64`), not to the version alone, since that whole string is
  what a registry has to accept.

They disagree in exactly two places, and the OCI side wins both:

| Version | Cargo | Here |
| --- | --- | --- |
| `1.2.3+build.5` | accepts | rejected — an OCI tag may not contain `+` |
| a 130-character prerelease | accepts | rejected — the tag exceeds 128 characters |

Build metadata is rejected rather than stripped: a container tag that silently
disagrees with the git tag is worse than a release that refuses to start. Note
also *where* the unfixed version of this failed — `docker` runs after `release`,
so an unrepresentable version would have produced a published tag with assets
and no images.
