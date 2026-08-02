#!/usr/bin/env bash
# Remove save_audio_stream installed by install.sh.
#
#   uninstall.sh            remove the launcher and every installed version,
#                           KEEPING <prefix>/etc and <prefix>/data
#   uninstall.sh <version>  remove just that version, deactivating `current` if
#                           it pointed there
#   uninstall.sh --purge    remove <prefix> entirely, config and recordings
#                           included (prompts unless --yes is also given)
#
#   PREFIX   install root   (default: /opt/save_audio_stream)
#   BINDIR   launcher dir   (default: /usr/local/bin)
#
# Config and recordings are kept by default deliberately. Unlike the versioned
# directories they are not reproducible from a release artifact: `rm -rf`-ing a
# week of recordings because someone typed `uninstall.sh` is not a recoverable
# mistake. `--purge` is the way to say you meant it.
#
# Use sudo if the directories are not writable by your user.
set -euo pipefail

prefix="${PREFIX:-/opt/save_audio_stream}"
bindir="${BINDIR:-/usr/local/bin}"

purge=0
assume_yes=0
version=""
for arg in "$@"; do
  case "$arg" in
    --purge) purge=1 ;;
    --yes|-y) assume_yes=1 ;;
    -*) echo "error: unknown option: $arg" >&2; exit 1 ;;
    *)
      if [ -n "$version" ]; then
        echo "error: only one version may be given" >&2
        exit 1
      fi
      version="$arg"
      ;;
  esac
done

if [ "$purge" = 1 ] && [ -n "$version" ]; then
  echo "error: --purge removes everything; it takes no version argument" >&2
  exit 1
fi

remove_launcher() {
  # Only remove the launcher if it points into our prefix — another install
  # elsewhere may own that name.
  local launcher="$bindir/save_audio_stream"
  if [ -L "$launcher" ] && [ "$(readlink "$launcher")" = "$prefix/current/bin/save_audio_stream" ]; then
    rm -f "$launcher"
    echo ">> removed $launcher"
  fi
}

if [ -n "$version" ]; then
  # Single version: the launcher normally stays, because the version still
  # installed alongside this one continues to need it.
  if [ "$(readlink "$prefix/current" 2>/dev/null)" = "versions/$version" ]; then
    rm -f "$prefix/current"
    echo ">> deactivated current (was $version)"
    # ...but this was the *active* version, so `current` is gone and the
    # launcher now points through a symlink that resolves to nothing. Leaving it
    # would put a save_audio_stream on PATH that fails with "No such file or
    # directory". Remove it; re-pointing `current` at a kept version and
    # re-running install.sh restores it.
    remove_launcher
  fi
  rm -rf "${prefix:?}/versions/$version"
  echo ">> removed version $version"
  exit 0
fi

if [ "$purge" = 1 ]; then
  if [ "$assume_yes" != 1 ]; then
    echo "This removes $prefix entirely, including:"
    echo "  $prefix/etc   (configuration and credentials)"
    echo "  $prefix/data  (recordings and their databases)"
    printf 'Type "purge" to confirm: '
    read -r reply
    [ "$reply" = "purge" ] || { echo "aborted"; exit 1; }
  fi
  remove_launcher
  rm -rf "${prefix:?}"
  echo ">> removed $prefix"
  exit 0
fi

remove_launcher
rm -rf "${prefix:?}/versions" "$prefix/current" "$prefix/.install.lock"
echo ">> removed all installed versions from $prefix"
echo ">> kept $prefix/etc   (configuration and credentials)"
echo ">> kept $prefix/data  (recordings and their databases)"
echo ">> to remove those too: $(basename "$0") --purge"
