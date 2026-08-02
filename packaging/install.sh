#!/usr/bin/env bash
# Install save_audio_stream from an extracted release tarball.
#
# Layout (distro-agnostic, works on Linux and macOS):
#
#   <prefix>/etc/{record,receiver,credentials}.toml   # stable user configuration
#   <prefix>/data/recordings                          # recordings, SQLite DBs, locks
#   <prefix>/versions/<version>/{bin,share}           # this version's files
#   <prefix>/current -> versions/<version>            # active version (atomic swap)
#   <bindir>/save_audio_stream -> <prefix>/current/bin/save_audio_stream
#
# Upgrade model:
#   1. The new version is staged fully into versions/<version> before anything
#      user-visible changes.
#   2. `current` is flipped to it with an atomic rename(2), so the launcher
#      never observes a half-installed version — a running recorder keeps
#      running the version it started from.
#   3. Older versions are pruned, keeping only the new one and the immediately
#      previous one (for rollback: point `current` back at it).
#
# etc/ and data/ are siblings of versions/, not children: the pruning loop below
# only ever walks <prefix>/versions/*/, and `current` only ever names a
# directory in there, so neither config nor recordings are touched by an
# upgrade, a prune or a rollback. That is also what makes the per-show .lock
# files work across an upgrade — the old and new binaries contend on one path
# rather than each taking its own.
#
# Env overrides:
#   PREFIX   install root   (default: /opt/save_audio_stream)
#   BINDIR   launcher dir   (default: /usr/local/bin)
#
# Run from inside the extracted save_audio_stream-<version>/ directory. Use sudo
# if the target directories are not writable by your user.
set -euo pipefail

src="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
prefix="${PREFIX:-/opt/save_audio_stream}"
bindir="${BINDIR:-/usr/local/bin}"

[ -f "$src/VERSION" ] || {
  echo "error: run this from an extracted save_audio_stream release" >&2
  exit 1
}
version="$(cat "$src/VERSION")"

# Serialize installs against this prefix. `mkdir` is atomic on POSIX, so it
# doubles as a portable lock (flock isn't on stock macOS). The lock is released
# on any exit; a leftover lock means another install is in progress.
mkdir -p "$prefix"
lock="$prefix/.install.lock"
if ! mkdir "$lock" 2>/dev/null; then
  echo "error: another install is running (lock: $lock). Remove it if stale." >&2
  exit 1
fi
trap 'rm -rf "$lock" "${staging:-}"' EXIT

# Atomically replace the symlink `$2` with a new symlink to `$1`, without ever
# dereferencing an existing symlink-to-directory. GNU coreutils uses `-T`; BSD
# (macOS) uses `-h`. Both resolve to a single rename(2).
swap_symlink() {
  local target="$1" link="$2" tmp
  tmp="$(dirname "$link")/.$(basename "$link").new.$$"
  ln -s "$target" "$tmp"
  if mv --version >/dev/null 2>&1; then
    mv -Tf "$tmp" "$link"
  else
    mv -fh "$tmp" "$link"
  fi
}

# What is active right now, before we touch anything — this becomes "previous".
prev_link="$(readlink "$prefix/current" 2>/dev/null || true)"   # e.g. versions/0.2.14
prev_version="${prev_link#versions/}"

# Stage the new version into a temp dir, then publish it in one move so a
# partial copy is never named versions/<version>.
staging="$prefix/versions/.incoming.$version.$$"
final="$prefix/versions/$version"
mkdir -p "$prefix/versions"

echo ">> staging save_audio_stream $version"
rm -rf "$staging"
mkdir -p "$staging"
cp -R "$src/bin" "$src/share" "$staging/"
cp "$src/VERSION" "$staging/VERSION"

config_dir="$prefix/etc"
mkdir -p "$config_dir"

# Seed each config once and never again: an existing file is the operator's, and
# a reinstall that overwrote it would take their stream list or their database
# password with it.
#
# All three are 0600, not just credentials.toml — receiver.toml carries a
# Postgres URL and record.toml can carry stream URLs with tokens in them. One
# uniform rule is also one less rule to get wrong.
seed_config() {
  local name="$1"
  local dest="$config_dir/$name"
  local sample="$src/share/doc/save_audio_stream/$name.example"

  if [ -e "$dest" ] && [ ! -f "$dest" ]; then
    echo "error: config path is not a regular file: $dest" >&2
    exit 1
  fi
  if [ ! -f "$dest" ]; then
    echo ">> seeding $name from sample — edit $dest"
    cp "$sample" "$dest"
  else
    echo ">> preserving $name at $dest"
  fi
  if [ -n "${SUDO_UID:-}" ] && [ -n "${SUDO_GID:-}" ]; then
    chown "$SUDO_UID:$SUDO_GID" "$dest"
  fi
  if ! chmod 600 "$dest"; then
    echo "error: could not set 600 permissions on $dest" >&2
    echo "       refusing to install — credentials would not be secured." >&2
    exit 1
  fi
}

# All three regardless of which subcommand this machine will run: finding the
# file is how you find out the mode exists, and they are inert until read.
seed_config record.toml
seed_config receiver.toml
seed_config credentials.toml

if [ -n "${SUDO_UID:-}" ] && [ -n "${SUDO_GID:-}" ]; then
  chown "$SUDO_UID:$SUDO_GID" "$config_dir"
fi
chmod 700 "$config_dir"

# Beside versions/, never inside one — see the header. Created here so a fresh
# install has somewhere to record to before anyone edits a config.
#
# The chown runs only on the install that creates the directory. Doing it on
# every upgrade would walk the whole recording tree — potentially tens of
# gigabytes — and, worse, would silently reassign ownership of every recording
# to whoever ran sudo, undoing a deliberate choice to run the recorder as a
# dedicated service account.
data_dir="$prefix/data/recordings"
if [ ! -d "$data_dir" ]; then
  mkdir -p "$data_dir"
  if [ -n "${SUDO_UID:-}" ] && [ -n "${SUDO_GID:-}" ]; then
    chown -R "$SUDO_UID:$SUDO_GID" "$prefix/data"
  fi
fi
chmod 700 "$prefix/data"

# Publish the version directory (replace any same-version dir from a prior run).
rm -rf "$final"
mv "$staging" "$final"

# Flip the active version atomically, then ensure the launcher exists (stable —
# it always follows `current`, so upgrades don't touch it).
echo ">> activating $version"
swap_symlink "versions/$version" "$prefix/current"
mkdir -p "$bindir"
ln -sfn "$prefix/current/bin/save_audio_stream" "$bindir/save_audio_stream"

# Keep only the active version and the immediately previous one; remove the rest.
for dir in "$prefix"/versions/*/; do
  [ -d "$dir" ] || continue
  name="$(basename "$dir")"
  case "$name" in
    "$version"|"$prev_version") : ;;
    *) echo ">> removing old version $name"; rm -rf "$dir" ;;
  esac
done

echo ">> installed. 'save_audio_stream' -> $bindir/save_audio_stream -> $prefix/current/bin/save_audio_stream"
if [ -n "$prev_version" ] && [ "$prev_version" != "$version" ]; then
  echo ">> previous version $prev_version kept for rollback:"
  echo "     ln -sfn versions/$prev_version $prefix/current"
  echo "   NOTE: rollback restores the binary, not the data. If this release"
  echo "         bumped the SQLite schema version, the old binary will refuse"
  echo "         the databases in $data_dir — see packaging/README.md."
fi
echo ">> config: $config_dir/{record,receiver,credentials}.toml"
echo ">> data:   $data_dir"
echo ">> run:    save_audio_stream record"
