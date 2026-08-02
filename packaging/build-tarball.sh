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

# Real TOML parse + semver check, not grep/sed — the same code the release
# workflow runs, so the tarball name and the git tag cannot disagree. Plain
# python3 (3.11+ for tomllib) so this also runs on CI runners without uv.
# `[package]`, not `[workspace.package]`: this is a single crate.
version="$(python3 -c '
import re, sys, tomllib
with open("Cargo.toml", "rb") as f:
    version = tomllib.load(f)["package"]["version"]
# Cargo accepts SemVer build metadata (1.2.3+build.5) but this pipeline cannot
# carry it: an OCI image tag may not contain "+" at all -- docker rejects
# ghcr.io/...:v1.2.3+build.5-amd64 with "invalid reference format" -- and the
# release asset filename is reconstructed character-for-character by the network
# install.sh, so anything the upload rewrites breaks the download. Rejected
# rather than sanitized, because a container tag that silently disagrees with
# the git tag is worse than a release that will not start.
if "+" in version:
    sys.exit(f"build metadata is not supported in release versions: {version!r}")
if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
    sys.exit(f"invalid version in Cargo.toml: {version!r}")
print(version)
')"

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
# fdk-aac vendors C sources, so this needs a C compiler; every CI runner has
# one. libopus does not — Cargo.toml uses opus-prebuilt, which pulls a prebuilt
# static archive rather than compiling vendored C through cmake.
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
