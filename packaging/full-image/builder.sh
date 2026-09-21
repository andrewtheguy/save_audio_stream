#!/usr/bin/env bash
# The half of publish-full-image.sh that runs on the machine doing the building, as the
# `ci/unix/ci.sh` of the staging tree that script ships through the devtools remote driver:
#
#   params            TAG, VERSION, COMMIT and IMAGE, as shell assignments
#   source/           the release tag's source
#   frontend-dist/    the frontend bundle, built once by the publisher
#   fdk-aac/<target>/ the private fdk-aac archive for each architecture, unpacked
#
# It compiles the binary with `--features aac` and builds the runtime image from the tag's own
# packaging/Dockerfile, for this machine's architecture and no other, then leaves the image in
# out/ as an OCI archive for the publisher to fetch and push. It pushes nothing, so the
# machine needs no registry credentials — which matters, because the arm64 builder is a
# production server.
#
# Everything happens in podman containers: the machine needs podman and nothing else, and
# the compiler is the image's, not the machine's. rust:1-trixie because the runtime image is
# debian:trixie-slim and a binary cannot need a newer glibc than the image it ships in.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
work="$PWD"
# shellcheck source=/dev/null
. ./params

case "$(uname -m)" in
  x86_64)          arch=amd64; fdk=linux-x86_64-v3 ;;
  aarch64 | arm64) arch=arm64; fdk=linux-aarch64 ;;
  *) echo "no image is built for $(uname -m)" >&2; exit 1 ;;
esac
[ -f "fdk-aac/$fdk/MANIFEST" ] || { echo "the staging tree has no fdk-aac/$fdk" >&2; exit 1; }

# A rootless podman reached over ssh has no login session, so no user bus — and its default
# cgroup manager asks systemd for a scope over exactly that bus, which fails every RUN step
# of a build with "Interactive authentication required".
podman=(podman)
[ -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bus" ] || podman+=(--cgroup-manager=cgroupfs)
# The host's network: all a build needs is a way out, and a rootless podman without `pasta`
# installed has no other.
network=(--network host)

# Outside the workspace, which the driver replaces on every run, so a second build is warm.
cache="${CARGO_TARGET_DIR:-$work/target}"
mkdir -p "$cache/target" "$cache/registry" out context/bin context/share/doc/save_audio_stream

echo ">> compiling save_audio_stream ${VERSION} with --features aac for linux/${arch}"
# shellcheck disable=SC2016  # the script is the container's to expand
"${podman[@]}" run --rm "${network[@]}" \
  -v "$work:/work" -v "$cache/target:/cargo-target" -v "$cache/registry:/usr/local/cargo/registry" \
  -e CARGO_TARGET_DIR=/cargo-target \
  -e FDK_AAC_PREBUILT_DIR="/work/fdk-aac/$fdk" \
  -e SAVE_AUDIO_STREAM_PREBUILT_FRONTEND=/work/frontend-dist \
  -w /work/source \
  docker.io/library/rust:1-trixie \
  bash -euo pipefail -c '
    cargo build --release --locked --features aac
    cp /cargo-target/release/save_audio_stream /work/context/bin/save_audio_stream
    got="$(/work/context/bin/save_audio_stream --version)"
    echo "   built: $got"
  '

for name in record receiver credentials; do
  cp "source/packaging/etc/${name}.toml.example" "context/share/doc/save_audio_stream/${name}.toml.example"
done

echo ">> building ${IMAGE}:${TAG}-${arch}"
"${podman[@]}" build "${network[@]}" \
  -f source/packaging/Dockerfile \
  --build-arg "VERSION=${VERSION}" \
  --label "org.opencontainers.image.revision=${COMMIT}" \
  -t "${IMAGE}:${TAG}-${arch}" \
  context

# The same smoke test the release workflow gives the public image: the binary runs on this
# base, and reports the version the tag names.
got="$("${podman[@]}" run --rm "${network[@]}" "${IMAGE}:${TAG}-${arch}" --version)"
echo "   image reports: $got"
[ "$got" = "save_audio_stream ${VERSION}" ] \
  || { echo "expected 'save_audio_stream ${VERSION}'" >&2; exit 1; }

# And the feature, which `--version` does not show. A build without the encoder refuses an
# `aac` session before it looks at the URL; this one must get as far as failing to reach it.
cat > context/aac-check.toml <<'TOML'
config_type = 'record'
output_dir = '/tmp/aac-check'

[[sessions]]
url = 'http://127.0.0.1:9/none'
name = 'aac-check'
audio_format = 'aac'

[sessions.schedule]
record_start = "00:00"
record_end = "23:59"
TOML
said="$("${podman[@]}" run --rm "${network[@]}" \
  -v "$work/context/aac-check.toml:/opt/save_audio_stream/etc/record.toml:ro" \
  "${IMAGE}:${TAG}-${arch}" record 2>&1 || true)"
case "$said" in
  *"no AAC encoder"*) echo "the image was built without the aac feature" >&2; exit 1 ;;
  *"Testing stream URL"*) echo "   image records aac" ;;
  *) echo "the aac check did not reach the URL test:" >&2; echo "$said" >&2; exit 1 ;;
esac

rm -f "out/image-${arch}.tar"
"${podman[@]}" save --format oci-archive -o "out/image-${arch}.tar" "${IMAGE}:${TAG}-${arch}"
"${podman[@]}" rmi "${IMAGE}:${TAG}-${arch}" >/dev/null
ls -l "out/image-${arch}.tar"
