//! The model library: the bundled characters plus the user's own models.
//!
//! The engine never hard-codes a particular character. It addresses models by
//! a library id: one of the bundled ids ([`BUILTIN`]) or the file stem of a
//! `*.dmodel.json` file in `%APPDATA%\Delicium\models\`. A library id looks
//! like [`crate::preferences::valid_id`] describes, so it can travel through
//! the command line and the configuration file without quoting rules.
//!
//! Scanning is shallow and bounded: the directory is not walked recursively
//! and at most [`MAX_USER_MODELS`] user entries are listed, so a large or
//! deliberately hostile directory cannot stall startup.

use std::path::{Path, PathBuf};

use crate::preferences::valid_id;

/// Maximum number of user models listed.
pub const MAX_USER_MODELS: usize = 200;

/// A library id and how to find its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryModel {
    pub id: String,
    /// The character's display name: the bundled name, or the file stem
    /// (without `.dmodel.json`) for a user model.
    pub name: String,
    pub source: Source,
}

/// Where a library model's bytes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Compiled into the binary.
    Bundled,
    /// A file in the user's model directory.
    File(PathBuf),
}

/// The bundled models and their display names. Order is the order the
/// library lists them; the first entry is the default pet.
pub const BUILTIN: &[(&str, &str)] = &[
    ("bangboo-eous", "Eous"),
    ("puppet2d", "Puppet"),
    ("robot3d", "Robot"),
    ("puppet2d-textured", "Puppet (textured)"),
    ("robot3d-textured", "Robot (textured)"),
];

/// The bundled model with this id, if any.
pub fn builtin(id: &str) -> Option<LibraryModel> {
    BUILTIN
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(id, name)| LibraryModel {
            id: (*id).to_owned(),
            name: (*name).to_owned(),
            source: Source::Bundled,
        })
}

/// `%APPDATA%\Delicium\models`; `None` without an `APPDATA` variable.
pub fn user_directory() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("Delicium").join("models"))
}

/// Lists the user models in `directory`, sorted by name.
///
/// A user file whose stem collides with a bundled id is ignored: the bundled
/// asset keeps its meaning, so a stray file in the user directory can never
/// replace the shipped character. Files that are not plain `*.dmodel.json`,
/// and stems that are not valid ids, are skipped.
pub fn list_user(directory: &Path) -> Vec<LibraryModel> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for entry in entries.flatten() {
        if models.len() >= MAX_USER_MODELS {
            break;
        }
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(stem) = model_stem(&path) else {
            continue;
        };
        if !valid_id(stem) || builtin(stem).is_some() {
            continue;
        }
        if models.iter().any(|known: &LibraryModel| known.id == stem) {
            continue;
        }
        models.push(LibraryModel {
            id: stem.to_owned(),
            name: stem.to_owned(),
            source: Source::File(path),
        });
    }
    models.sort_by_key(|model| model.name.to_lowercase());
    models
}

/// Resolves `id` against the library. `user_directory` is where user models
/// live; a `File` source is only returned for a file that exists.
pub fn resolve(id: &str, user_directory: Option<&Path>) -> Option<LibraryModel> {
    if let Some(model) = builtin(id) {
        return Some(model);
    }
    if !valid_id(id) {
        return None;
    }
    let path = user_directory?.join(format!("{id}.dmodel.json"));
    if !path.is_file() {
        return None;
    }
    Some(LibraryModel {
        id: id.to_owned(),
        name: id.to_owned(),
        source: Source::File(path),
    })
}

/// Every model reachable through the library.
pub fn list(user_directory: Option<&Path>) -> Vec<LibraryModel> {
    let mut models: Vec<LibraryModel> = BUILTIN
        .iter()
        .map(|(id, _)| builtin(id).expect("builtin table is consistent"))
        .collect();
    if let Some(directory) = user_directory {
        let remaining = MAX_USER_MODELS.saturating_sub(models.len() - BUILTIN.len());
        models.extend(list_user(directory).into_iter().take(remaining));
    }
    models
}

/// `X.dmodel.json` -> `X`. The stem may still contain dots, e.g.
/// `my.pet.dmodel.json` names `my.pet`; that is rejected by [`valid_id`]
/// later, never silently accepted.
fn model_stem(path: &Path) -> Option<&str> {
    let file_name = path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".dmodel.json")?;
    (!stem.is_empty()).then_some(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "delicium-library-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn builtin_ids_are_unique_valid_and_include_the_pet() {
        let mut seen = std::collections::BTreeSet::new();
        for (id, name) in BUILTIN {
            assert!(valid_id(id), "{id}");
            assert!(seen.insert(*id), "duplicate {id}");
            assert!(!name.is_empty());
        }
        let pet = builtin("bangboo-eous").expect("the pet is a bundled model");
        assert_eq!(pet.source, Source::Bundled);
        assert!(builtin("../escape").is_none());
        assert!(builtin("Bangboo").is_none());
    }

    #[test]
    fn the_user_directory_lists_only_valid_model_files() {
        let directory = test_dir("list");
        for name in [
            "puppet2d.dmodel.json", // collides with a bundled id
            "my-pet.dmodel.json",
            "other_model.dmodel.json",
            "with space.dmodel.json", // invalid id
            "notes.txt",
            ".dmodel.json",
            "nested.dmodel.json.dmodel.json",
        ] {
            std::fs::write(directory.join(name), b"{}").unwrap();
        }
        let nested = directory.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("deep.dmodel.json"), b"{}").unwrap();
        let models = list_user(&directory);
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["my-pet", "other_model"]);
        assert!(matches!(models[0].source, Source::File(_)));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_missing_directory_is_an_empty_library_not_an_error() {
        assert!(list_user(Path::new("Z:\\delicium-no-such-directory")).is_empty());
    }

    #[test]
    fn resolution_prefers_bundled_models_over_user_files() {
        let directory = test_dir("resolve");
        std::fs::write(directory.join("bangboo-eous.dmodel.json"), b"{}").unwrap();
        std::fs::write(directory.join("custom.dmodel.json"), b"{}").unwrap();
        let pet = resolve("bangboo-eous", Some(&directory)).unwrap();
        assert_eq!(pet.source, Source::Bundled);
        let custom = resolve("custom", Some(&directory)).unwrap();
        assert_eq!(
            custom.source,
            Source::File(directory.join("custom.dmodel.json"))
        );
        assert!(resolve("missing", Some(&directory)).is_none());
        assert!(resolve("../custom", Some(&directory)).is_none());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn listing_appends_user_models_after_the_bundled_ones() {
        let directory = test_dir("full");
        std::fs::write(directory.join("zzz.dmodel.json"), b"{}").unwrap();
        let models = list(Some(&directory));
        assert_eq!(models.len(), BUILTIN.len() + 1);
        assert_eq!(models.last().unwrap().id, "zzz");
        assert_eq!(list(None).len(), BUILTIN.len());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
