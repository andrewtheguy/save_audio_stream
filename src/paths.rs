//! Every filesystem location the program derives rather than is told.
//!
//! Two kinds: where config files are, and where recordings go. Each resolves the same way — an explicit environment
//! override, then the installed layout inferred from the running binary's own
//! path, then a per-user fallback.

use std::path::PathBuf;

/// Used for the per-user fallback directories and the Windows ProgramData tree.
const APP_NAME: &str = "save_audio_stream";

/// The root the binary was installed under: two levels above the *real*
/// executable.
///
/// - unix:    `<prefix>/versions/<version>/bin/save_audio_stream` → `<prefix>/versions/<version>`
/// - windows: `C:\Program Files\save_audio_stream\bin\save_audio_stream.exe` → `C:\Program Files\save_audio_stream`
/// - checkout: `target/debug/save_audio_stream` → `target` (harmless: no `share/` there)
///
/// `canonicalize` is what makes the `/usr/local/bin` launcher resolve through
/// both its own symlink and the `current` symlink to the real versioned
/// directory, rather than stopping at `/usr/local`.
pub fn install_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    Some(exe.parent()?.parent()?.to_path_buf())
}

/// The install prefix on unix: `<install_root>/../..`, but only when the middle
/// component is literally named `versions`.
///
/// Without that check a binary run from `target/release/` would invent a prefix
/// out of whatever happened to sit two directories up, and start reading a
/// stranger's config.
#[cfg(unix)]
fn installed_prefix() -> Option<PathBuf> {
    let root = install_root()?; // <prefix>/versions/<version>
    let versions = root.parent()?; // <prefix>/versions
    if versions.file_name()? != "versions" {
        return None;
    }
    Some(versions.parent()?.to_path_buf())
}

/// `<prefix>/etc` when running from the versioned install layout.
#[cfg(unix)]
fn installed_config_dir() -> Option<PathBuf> {
    Some(installed_prefix()?.join("etc"))
}

/// `<prefix>/data` when running from the versioned install layout.
#[cfg(unix)]
fn installed_data_dir() -> Option<PathBuf> {
    Some(installed_prefix()?.join("data"))
}

/// `%ProgramData%\save_audio_stream\etc`.
///
/// Unconditional, unlike the unix side: Program Files is not user-writable, so
/// config can never live beside the executable on Windows and there is no
/// layout to detect.
#[cfg(windows)]
fn installed_config_dir() -> Option<PathBuf> {
    Some(program_data()?.join("etc"))
}

/// `%ProgramData%\save_audio_stream\data`. See [`installed_config_dir`].
#[cfg(windows)]
fn installed_data_dir() -> Option<PathBuf> {
    Some(program_data()?.join("data"))
}

#[cfg(windows)]
fn program_data() -> Option<PathBuf> {
    let base = std::env::var_os("ProgramData")?;
    Some(PathBuf::from(base).join(APP_NAME))
}

/// The directory holding `record.toml`, `receiver.toml` and `credentials.toml`.
///
/// 1. `SAVE_AUDIO_STREAM_CONFIG_DIR`
/// 2. installed: `<prefix>/etc` (unix) or `%ProgramData%\save_audio_stream\etc` (windows)
/// 3. per-user: `dirs::config_dir()/save_audio_stream`
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SAVE_AUDIO_STREAM_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = installed_config_dir() {
        return dir;
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
}

/// A named config file inside [`config_dir`].
pub fn config_path(file_name: &str) -> PathBuf {
    config_dir().join(file_name)
}

/// The directory holding mutable state.
///
/// On an installed unix system this is `<prefix>/data` — a *sibling* of
/// `versions/`, deliberately: the installer's pruning loop only ever walks
/// `<prefix>/versions/*/`, and rollback only ever re-points the `current`
/// symlink, so nothing here is touched by either. Deriving it from
/// [`install_root`] instead would make recordings roll with the binary.
///
/// 1. `SAVE_AUDIO_STREAM_DATA_DIR`
/// 2. installed: `<prefix>/data` (unix) or `%ProgramData%\save_audio_stream\data` (windows)
/// 3. per-user: `dirs::data_dir()/save_audio_stream`
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SAVE_AUDIO_STREAM_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = installed_data_dir() {
        return dir;
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
}

/// Where recordings, their SQLite databases and their lock files go when the
/// config omits `output_dir`.
pub fn default_output_dir() -> PathBuf {
    data_dir().join("recordings")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not three: these all mutate process-wide environment, and the
    /// test harness runs test functions on parallel threads.
    #[test]
    fn env_overrides_win_and_compose() {
        // SAFETY: single-threaded within this test, and no other test in this
        // crate reads these variables.
        unsafe {
            std::env::set_var("SAVE_AUDIO_STREAM_CONFIG_DIR", "/tmp/sas-cfg");
            std::env::set_var("SAVE_AUDIO_STREAM_DATA_DIR", "/tmp/sas-data");
        }

        assert_eq!(config_dir(), PathBuf::from("/tmp/sas-cfg"));
        assert_eq!(
            config_path("record.toml"),
            PathBuf::from("/tmp/sas-cfg/record.toml")
        );
        assert_eq!(data_dir(), PathBuf::from("/tmp/sas-data"));
        // The recordings default is derived from the data directory, so an
        // override moves the databases and their lock files together.
        assert_eq!(
            default_output_dir(),
            PathBuf::from("/tmp/sas-data/recordings")
        );

        unsafe {
            std::env::remove_var("SAVE_AUDIO_STREAM_CONFIG_DIR");
            std::env::remove_var("SAVE_AUDIO_STREAM_DATA_DIR");
        }
    }

    /// The versioned-layout guard: a binary that is not at
    /// `<prefix>/versions/<v>/bin/` must not invent a prefix out of whatever
    /// sits two directories up. Under `cargo test` the executable lives in
    /// `target/debug/deps/`, so this exercises exactly that case.
    #[cfg(unix)]
    #[test]
    fn checkout_binary_does_not_claim_an_install_prefix() {
        assert_eq!(installed_prefix(), None);
    }
}
