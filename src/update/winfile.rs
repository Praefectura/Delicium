//! Small filesystem helpers the updater needs that `std` does not express:
//! a write-through rename that replaces an existing file.
//!
//! `std::fs::rename` replaces an existing file on Windows, but it does not
//! request write-through, so a crash right after the call can leave the
//! directory entry pointing at a file whose content never reached the disk.
//! Everything the updater commits is committed through [`replace_atomic`].

use std::path::Path;

/// Renames `source` over `destination`, requesting write-through.
///
/// Both paths must be on the same volume; the caller keeps them in one
/// installation root for exactly that reason.
pub fn replace_atomic(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

        fn wide(path: &Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        let source = wide(source);
        let destination = wide(destination);
        // SAFETY: both strings are NUL-terminated and live for the call; the
        // API copies them and touches no other memory.
        let ok = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::fs::rename(source, destination)
    }
}

/// Renames a directory into place, refusing to replace anything.
///
/// An existing destination means another installation already claimed the
/// version, which is a conflict to report rather than overwrite.
pub fn rename_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    if destination.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", destination.display()),
        ));
    }
    std::fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "delicium-winfile-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn replace_overwrites_and_leaves_one_file() {
        let directory = test_dir("replace");
        let source = directory.join("new.json");
        let destination = directory.join("state.json");
        std::fs::write(&source, "new").unwrap();
        std::fs::write(&destination, "old").unwrap();
        replace_atomic(&source, &destination).unwrap();
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "new");
        assert!(!source.exists());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn rename_new_refuses_an_existing_destination() {
        let directory = test_dir("rename-new");
        let source = directory.join("staging");
        let destination = directory.join("app");
        std::fs::create_dir(&source).unwrap();
        rename_new(&source, &destination).unwrap();
        assert!(destination.is_dir());
        std::fs::create_dir(&source).unwrap();
        let error = rename_new(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
