//! Where the launcher keeps the installation.
//!
//! One root directory holds the launcher itself, `state.json`, the versioned
//! `app\` directories, the staging area, backups and logs. It defaults to
//! `%LOCALAPPDATA%\Delicium` so the launcher never needs write access to the
//! folder it happened to be started from; `DELICIUM_INSTALL_ROOT` overrides
//! it for tests and portable setups.

use std::path::{Path, PathBuf};

/// Environment variable that moves the whole installation.
pub const ROOT_ENV: &str = "DELICIUM_INSTALL_ROOT";

/// The launcher binary, at the root of the installation.
pub const LAUNCHER_FILE: &str = "delicium-launcher.exe";

/// The engine binary, inside each version directory.
pub const ENGINE_FILE: &str = "delicium.exe";

pub const LOGS_DIR: &str = "logs";
pub const BACKUP_DIR: &str = "backup";
pub const STAGING_DIR: &str = "staging";
pub const APP_DIR: &str = "app";
pub const RECORD_FILE: &str = "launcher.json";

/// The engine's own configuration file, under `%APPDATA%`.
///
/// The launcher writes this file and the engine reads it, so it lives where
/// the engine has always put it, not under the installation root. Keeping it
/// outside the root also means reinstalling or rolling back never resets the
/// user's settings.
pub fn config_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join("Delicium").join("config.json"))
}

/// The installation root for this process.
pub fn install_root() -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os(ROOT_ENV) {
        let path = PathBuf::from(value);
        if path.as_os_str().is_empty() {
            return Err(format!("{ROOT_ENV} is set but empty"));
        }
        return Ok(path);
    }
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return Err(format!(
            "LOCALAPPDATA is not set; set {ROOT_ENV} to choose an installation directory"
        ));
    };
    Ok(PathBuf::from(local).join("Delicium"))
}

pub fn launcher_exe(root: &Path) -> PathBuf {
    root.join(LAUNCHER_FILE)
}

pub fn logs_dir(root: &Path) -> PathBuf {
    root.join(LOGS_DIR)
}

pub fn backup_dir(root: &Path) -> PathBuf {
    root.join(BACKUP_DIR)
}

pub fn record_path(root: &Path) -> PathBuf {
    root.join(RECORD_FILE)
}

/// Creates every directory the launcher writes into.
pub fn ensure_layout(root: &Path) -> Result<(), String> {
    for directory in [
        root.to_path_buf(),
        logs_dir(root),
        backup_dir(root),
        root.join(APP_DIR),
        root.join(STAGING_DIR),
    ] {
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("Cannot create {}: {error}", directory.display()))?;
    }
    Ok(())
}

/// A short, stable key for mutex names, derived from the root path.
///
/// Two launchers pointed at the same directory share the key; two
/// installations never collide.
pub fn install_key(root: &Path) -> String {
    let text = root.to_string_lossy().to_lowercase();
    let digest = crate::update::sha256::to_hex(&crate::update::sha256::digest(text.as_bytes()));
    digest[..16].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_stable_and_case_insensitive() {
        let a = install_key(Path::new(r"C:\Users\Example\AppData\Local\Delicium"));
        let b = install_key(Path::new(r"c:\users\example\appdata\local\delicium"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        let other = install_key(Path::new(r"D:\Elsewhere\Delicium"));
        assert_ne!(a, other);
        assert!(a.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn layout_paths_are_relative_to_the_root() {
        let root = Path::new(r"C:\x");
        assert_eq!(launcher_exe(root), root.join("delicium-launcher.exe"));
        assert_eq!(logs_dir(root), root.join("logs"));
        assert_eq!(record_path(root), root.join("launcher.json"));
    }
}
