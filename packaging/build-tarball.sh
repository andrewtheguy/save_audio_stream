#!/usr/bin/env bash
# Build a distro-agnostic release tarball for the current OS/arch.
#
# Produces dist/save_audio_stream-<version>-<os>-<arch>.tar.gz containing a
# relocatable tree that install.sh lays down under <prefix>/versions/<version>:
#
#   save_audio_stream-<version>/
#   ├── VERSION
#   ├── bin/save_audio_stream                        # release binary
#   ├── share/doc/save_audio_stream/*.toml.example   # config templates
#   ├── share/save_audio_stream/web/                 # built frontend
#   ├── install.sh
#   └── uninstall.sh
#
# Run on each target platform you want to ship — this does not cross-compile.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# The same validator the release workflow runs, so a version this accepts
# locally cannot be rejected in CI (or the reverse). See packaging/release_version.py
# for what "releasable" means and why.
version="$(python3 packaging/release_version.py)"

case "$(uname -s)" in
  Linux)  os=linux ;;
  Darwin) os=macos ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  arch=x86_64 ;;
  arm64|aarch64) arch=arm64 ;;
  *) arch="$(uname -m)" ;;
esac

pkg="save_audio_stream-${version}"
stage="$(mktemp -d)"
root="${stage}/${pkg}"
trap 'rm -rf "$stage"' EXIT

# The frontend bundle is platform-agnostic, so CI builds it once and reuses it
# across every target. Set SKIP_FRONTEND_BUILD=1 to use an existing
# frontend/dist instead of rebuilding (requires `bun run build` to have run).
if [ "${SKIP_FRONTEND_BUILD:-0}" = 1 ]; then
  [ -d frontend/dist ] || { echo "SKIP_FRONTEND_BUILD=1 but frontend/dist is missing" >&2; exit 1; }
  echo ">> using prebuilt frontend/dist"
else
  echo ">> building frontend"
  ( cd frontend && bun run build )
fi

echo ">> building release binary"
# Neither codec is compiled here: Cargo.toml takes libopus from opus-prebuilt
# and fdk-aac from fdk-aac-prebuilt, both of which pull a prebuilt static
# archive rather than compiling vendored sources. A C compiler is still needed
# for the other vendored C (SQLite, libssh2, zlib); every CI runner has one.
#
# build.rs would build the frontend a second time here. Harmless (vite is fast
# and the second run is a no-op-shaped rebuild), and CI sets CI=true, which
# makes build.rs skip it outright.
cargo build --release

echo ">> assembling ${pkg}"
mkdir -p "$root/bin" "$root/share/doc/save_audio_stream" "$root/share/save_audio_stream"
cp target/release/save_audio_stream "$root/bin/save_audio_stream"
for name in record receiver credentials; do
  cp "packaging/etc/${name}.toml.example" "$root/share/doc/save_audio_stream/${name}.toml.example"
done
cp -R frontend/dist "$root/share/save_audio_stream/web"
cp packaging/install.sh packaging/uninstall.sh "$root/"
chmod +x "$root/install.sh" "$root/uninstall.sh" "$root/bin/save_audio_stream"
printf '%s\n' "$version" > "$root/VERSION"

mkdir -p dist
tarball="dist/${pkg}-${os}-${arch}.tar.gz"
tar -czf "$tarball" -C "$stage" "$pkg"
echo ">> wrote $tarball"
