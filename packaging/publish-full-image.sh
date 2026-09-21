#!/usr/bin/env bash
# Build the image release CI never publishes — save_audio_stream with the `aac` feature, the
# AAC encoder no release artifact links — and push it to the operator's private registry,
# ghcr.io/andrewtheguy/save_audio_stream-full, under the release's own tag (v0.3.5), as the
# same kind of multi-arch manifest the public image is.
#
# The package must stay private: the feature is kept out of release artifacts because of the
# encoder's licence, and a public package is a release artifact. The first push creates it
# `internal` — readable by the organization — and only the package's settings page on GitHub
# can make it private; there is no API for it. Every push, this script stops if an anonymous
# client can read the image.
#
# It builds a release tag and nothing else, from the source archive GitHub serves for that
# tag: neither an unreleased commit nor anything in this checkout reaches the image, except
# packaging/full-image/builder.sh, which is this checkout's because a tag holds whatever
# script it was cut with. Nothing is built in a workflow: a public repository's artifacts
# are public, and the encoder's archives are not (see fdk-aac-prebuilt).
#
# Each architecture is compiled on a machine of that architecture, in podman containers, by
# builder.sh — shipped there and run by the sibling `devtools` checkout's unix remote driver
# (or `DEVTOOLS_DIR`), which brings back the image as an OCI archive. Pushing happens here,
# so a builder needs no registry credentials:
#
#   linux/amd64   this machine
#   linux/arm64   $SAVE_AUDIO_STREAM_FULL_ARM64_HOST, an ssh host with podman
#
# Log in first: `podman login ghcr.io` with a token that has `write:packages`, and `gh auth
# login` with an account that can read the private fdk-aac archives.
#
#   SAVE_AUDIO_STREAM_FULL_ARM64_HOST=<ssh host> packaging/publish-full-image.sh TAG
#
#   TAG  the release to build, e.g. v0.3.5
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

registry=ghcr.io
package=andrewtheguy/save_audio_stream-full
image="${registry}/${package}"
github=https://github.com/andrewtheguy/save_audio_stream
fdk_repo=andrewtheguy/fdk-aac-prebuilt-archives

if [ $# -ne 1 ] || [ "${1#-}" != "$1" ]; then echo "usage: $0 TAG" >&2; exit 2; fi
tag="$1"

[ "$(uname -s)-$(uname -m)" = Linux-x86_64 ] \
  || { echo "the publisher builds linux/amd64 itself: run it on Linux x86_64, not $(uname -s)-$(uname -m)" >&2; exit 1; }
arm_host="${SAVE_AUDIO_STREAM_FULL_ARM64_HOST:-}"
[ -n "$arm_host" ] \
  || { echo "set SAVE_AUDIO_STREAM_FULL_ARM64_HOST to the ssh host that builds linux/arm64" >&2; exit 1; }
export DEVTOOLS_DIR="${DEVTOOLS_DIR:-$repo_root/../devtools}"
[ -f "$DEVTOOLS_DIR/ci/unix/remote.sh" ] \
  || { echo "no devtools checkout at $DEVTOOLS_DIR (set DEVTOOLS_DIR)" >&2; exit 1; }

# Before the builds rather than after them.
podman login --get-login "$registry" >/dev/null 2>&1 \
  || { echo "not logged in to ${registry}: podman login ${registry}" >&2; exit 1; }
gh release view --repo "$fdk_repo" --json tagName >/dev/null \
  || { echo "cannot read $fdk_repo: gh auth login, with an account that has access" >&2; exit 1; }

work="$repo_root/tmp/full-image"
stage="$work/stage"
cleanup() { rm -rf "$stage" "$work/source.tar.gz" "$work/fdk" "$work/images"; }

# One build at a time: they share the staging tree, and each builder's workspace.
mkdir -p "$work"
exec 9>"$work/lock"
flock -n 9 || { echo "another full-image build is running in $work" >&2; exit 1; }
trap cleanup EXIT
cleanup
mkdir -p "$stage/source" "$stage/ci/unix" "$stage/fdk-aac" "$work/fdk" "$work/images"

echo ">> fetching the source of ${tag}"
curl -fsSL -o "$work/source.tar.gz" "${github}/archive/refs/tags/${tag}.tar.gz" \
  || { echo "${github} serves no source archive for a tag ${tag}" >&2; exit 1; }
# git archive records the commit in the tar's header, which is all of git the image's
# revision label needs. Not a pipe: git stops reading after the header, and pipefail would
# make gzip's SIGPIPE this script's failure.
commit="$(git get-tar-commit-id < <(gzip -dc "$work/source.tar.gz"))"
tar -xzf "$work/source.tar.gz" -C "$stage/source" --strip-components=1

version="$(cd "$stage/source" && python3 packaging/release_version.py)"
[ "v${version}" = "$tag" ] \
  || { echo "${tag} holds save_audio_stream ${version}; a release tag is v<its version>" >&2; exit 1; }
grep -q '^aac = ' "$stage/source/Cargo.toml" \
  || { echo "${tag} predates the aac feature; there is no full image of it to build" >&2; exit 1; }

# Once, here: the bundle is platform-independent, and build.rs takes it prebuilt, so no
# builder needs bun.
echo ">> building the frontend"
(cd "$stage/source/frontend" && bun install --frozen-lockfile && bun run build)
cp -R "$stage/source/frontend/dist" "$stage/frontend-dist"
rm -rf "$stage/source/frontend/node_modules"

# The encoder's archives are private and read through `gh`, whose login stays here: each
# builder gets its archive unpacked and is pointed at it with FDK_AAC_PREBUILT_DIR.
echo ">> fetching the fdk-aac archives"
gh release download --repo "$fdk_repo" --dir "$work/fdk" \
  --pattern SHA256SUMS --pattern 'fdk-aac-*-linux-x86_64-v3.tar.gz' --pattern 'fdk-aac-*-linux-aarch64.tar.gz'
(cd "$work/fdk" && sha256sum --check --ignore-missing --quiet SHA256SUMS)
for fdk in linux-x86_64-v3 linux-aarch64; do
  mkdir -p "$stage/fdk-aac/$fdk"
  tar -xzf "$work/fdk"/fdk-aac-*-"$fdk".tar.gz -C "$stage/fdk-aac/$fdk"
done

cp packaging/full-image/builder.sh "$stage/ci/unix/ci.sh"
printf 'TAG=%s\nVERSION=%s\nCOMMIT=%s\nIMAGE=%s\n' "$tag" "$version" "$commit" "$image" > "$stage/params"
# Its own name, so the builders' workspaces and caches are not the repository's own CI's.
printf 'REPO_NAME=save_audio_stream-full-image\nCI_ENV_PREFIX=SAVE_AUDIO_STREAM_FULL\n' > "$stage/.devtools.conf"

# Both machines at once; they have nothing to wait on each other for.
driver=(env "DEVTOOLS_REPO_ROOT=$stage" "$DEVTOOLS_DIR/ci/unix/remote.sh")
build() {
  local arch="$1"; shift
  "${driver[@]}" "$@" ci && "${driver[@]}" "$@" fetch out "$work/images/$arch"
}
echo ">> building linux/amd64 here and linux/arm64 on ${arm_host} (logs: $work/*.log)"
build amd64 > "$work/amd64.log" 2>&1 &
amd64_pid=$!
build arm64 -H "$arm_host" > "$work/arm64.log" 2>&1 &
arm64_pid=$!
failed=0
wait "$amd64_pid" || { echo "linux/amd64 FAILED — see $work/amd64.log" >&2; failed=1; }
wait "$arm64_pid" || { echo "linux/arm64 FAILED — see $work/arm64.log" >&2; failed=1; }
[ "$failed" = 0 ] || exit 1

for arch in amd64 arm64; do
  echo ">> pushing ${image}:${tag}-${arch}"
  podman load -q -i "$work/images/$arch/image-${arch}.tar" >/dev/null
  podman push "${image}:${tag}-${arch}"
done

echo ">> pushing the ${image}:${tag} manifest"
podman manifest rm "${image}:${tag}" >/dev/null 2>&1 || true
podman manifest create "${image}:${tag}" "docker://${image}:${tag}-amd64" "docker://${image}:${tag}-arm64"
podman manifest push --all "${image}:${tag}" "docker://${image}:${tag}"
podman manifest rm "${image}:${tag}" >/dev/null

# What a client with no credentials is told: a token that can pull means the package is
# public.
anonymous="$(curl -sS "https://${registry}/token?scope=repository:${package}:pull" \
  | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
status="$(curl -s -o /dev/null -w '%{http_code}' \
  -H "Authorization: Bearer ${anonymous}" \
  -H 'Accept: application/vnd.oci.image.index.v1+json' \
  -H 'Accept: application/vnd.docker.distribution.manifest.list.v2+json' \
  "https://${registry}/v2/${package}/manifests/${tag}")"
[ "$status" != 200 ] \
  || { echo "${image} is public: anyone can pull ${tag}. Make the package private" >&2; exit 1; }

echo ">> pushed ${image}:${tag} (${commit}) for linux/amd64 and linux/arm64; anonymous pull: HTTP ${status}"
