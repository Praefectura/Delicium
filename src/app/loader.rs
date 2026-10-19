use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

use crate::{
    library::{self, Source},
    model::{CompiledModel, LoadLimits, load_model_bytes, read_model_file},
};

pub(super) const QUEUE_CAPACITY: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LoadSource {
    Example2d,
    Example3d,
    TexturedExample2d,
    TexturedExample3d,
    /// A model addressed by library id. Bundled ids compile their bytes in;
    /// a user model carries the resolved file so the load does not re-scan
    /// the directory (and cannot pick a different file mid-load).
    Library {
        id: String,
        path: Option<PathBuf>,
    },
    File(PathBuf),
    ValidationProbe,
}

impl LoadSource {
    /// The loader source for a resolved library model.
    pub fn library(model: &library::LibraryModel) -> Self {
        Self::Library {
            id: model.id.clone(),
            path: match &model.source {
                Source::Bundled => None,
                Source::File(path) => Some(path.clone()),
            },
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Example2d => "Bundled 2D example".into(),
            Self::Example3d => "Bundled 3D example".into(),
            Self::TexturedExample2d => "Bundled textured 2D example".into(),
            Self::TexturedExample3d => "Bundled textured 3D example".into(),
            Self::Library { id, .. } => {
                library::builtin(id).map_or_else(|| id.clone(), |model| model.name)
            }
            Self::File(path) => path.display().to_string(),
            Self::ValidationProbe => "Smoke-test invalid model".into(),
        }
    }

    pub fn example_index(&self) -> Option<usize> {
        match self {
            Self::Example2d => Some(0),
            Self::Example3d => Some(1),
            Self::TexturedExample2d => Some(2),
            Self::TexturedExample3d => Some(3),
            Self::Library { .. } | Self::File(_) | Self::ValidationProbe => None,
        }
    }

    /// The library id this source was loaded from, bundled or user.
    ///
    /// Only the tests inspect the id directly; production code compares whole
    /// sources (the file behind a user model is part of that identity).
    #[cfg(test)]
    pub fn library_id(&self) -> Option<&str> {
        match self {
            Self::Library { id, .. } => Some(id),
            _ => None,
        }
    }

    fn load(&self, limits: &LoadLimits) -> Result<Arc<CompiledModel>, String> {
        let result = match self {
            Self::Example2d => load_model_bytes(
                include_bytes!("../../assets/examples/puppet2d.dmodel.json"),
                limits,
            ),
            Self::Example3d => load_model_bytes(
                include_bytes!("../../assets/examples/robot3d.dmodel.json"),
                limits,
            ),
            Self::TexturedExample2d => load_model_bytes(
                include_bytes!("../../assets/examples/puppet2d-textured.dmodel.json"),
                limits,
            ),
            Self::TexturedExample3d => load_model_bytes(
                include_bytes!("../../assets/examples/robot3d-textured.dmodel.json"),
                limits,
            ),
            Self::Library { id, path } => match path {
                Some(path) => {
                    let text = path.as_os_str().to_string_lossy();
                    if text.starts_with("\\\\") || text.starts_with("//") {
                        return Err("Only local model paths are supported in this prototype".into());
                    }
                    read_model_file(path, limits)
                }
                None => load_bundled(id, limits),
            },
            Self::File(path) => {
                let text = path.as_os_str().to_string_lossy();
                if text.starts_with("\\\\") || text.starts_with("//") {
                    return Err("Only local model paths are supported in this prototype".into());
                }
                read_model_file(path, limits)
            }
            Self::ValidationProbe => {
                load_model_bytes(b"{\"format_version\":\"unsupported\"}", limits)
            }
        };
        result.map_err(|error| error.to_string())
    }
}

/// The compiled-in bytes of a bundled library model.
///
/// This is the only place that pairs a bundled id with an asset file; every
/// other module treats the pet like any other library entry, so replacing the
/// shipped character is a table change here, not an engine change.
fn load_bundled(
    id: &str,
    limits: &LoadLimits,
) -> Result<Arc<CompiledModel>, crate::model::ModelError> {
    match id {
        "bangboo-eous" => load_model_bytes(
            include_bytes!("../../assets/examples/bangboo-eous.dmodel.json"),
            limits,
        ),
        "puppet2d" => load_model_bytes(
            include_bytes!("../../assets/examples/puppet2d.dmodel.json"),
            limits,
        ),
        "robot3d" => load_model_bytes(
            include_bytes!("../../assets/examples/robot3d.dmodel.json"),
            limits,
        ),
        "puppet2d-textured" => load_model_bytes(
            include_bytes!("../../assets/examples/puppet2d-textured.dmodel.json"),
            limits,
        ),
        "robot3d-textured" => load_model_bytes(
            include_bytes!("../../assets/examples/robot3d-textured.dmodel.json"),
            limits,
        ),
        other => Err(crate::model::ModelError::Io {
            path: PathBuf::from(other),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "unknown bundled model id"),
        }),
    }
}

pub(super) struct LoadJob {
    pub id: u64,
    pub source: LoadSource,
}

pub(super) struct CompletedLoad {
    pub id: u64,
    pub source: LoadSource,
    pub result: Result<Arc<CompiledModel>, String>,
}

pub(super) struct Loader {
    requests: SyncSender<LoadJob>,
    completed: Receiver<CompletedLoad>,
}

impl Loader {
    pub fn new() -> Result<Self, String> {
        let (requests, incoming) = mpsc::sync_channel::<LoadJob>(QUEUE_CAPACITY);
        let (outgoing, completed) = mpsc::sync_channel(QUEUE_CAPACITY);
        thread::Builder::new()
            .name("delicium-model-loader".into())
            .spawn(move || {
                let limits = LoadLimits::default();
                while let Ok(job) = incoming.recv() {
                    let result = std::panic::catch_unwind(|| job.source.load(&limits))
                        .unwrap_or_else(|_| Err("Model loading worker panicked".into()));
                    if outgoing
                        .send(CompletedLoad {
                            id: job.id,
                            source: job.source,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| format!("Cannot start model loading worker: {error}"))?;
        Ok(Self {
            requests,
            completed,
        })
    }

    pub fn submit(&self, job: LoadJob) -> Result<(), String> {
        self.requests.try_send(job).map_err(|error| match error {
            TrySendError::Full(_) => "Model loading queue is full; try again shortly".into(),
            TrySendError::Disconnected(_) => "Model loading worker has stopped".into(),
        })
    }

    pub fn poll(&self) -> Result<Option<CompletedLoad>, String> {
        match self.completed.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("Model loading worker has stopped".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_sources_keep_legacy_and_textured_cache_slots_separate() {
        let sources = [
            LoadSource::Example2d,
            LoadSource::Example3d,
            LoadSource::TexturedExample2d,
            LoadSource::TexturedExample3d,
        ];
        for (index, source) in sources.iter().enumerate() {
            assert_eq!(source.example_index(), Some(index));
            let model = source.load(&LoadLimits::default()).unwrap();
            assert_eq!(model.images.is_empty(), index < 2);
        }
        assert_eq!(
            LoadSource::File(PathBuf::from("local.dmodel.json")).example_index(),
            None
        );
        assert_eq!(LoadSource::ValidationProbe.example_index(), None);
    }

    #[test]
    fn library_sources_have_no_example_slot_and_their_own_label() {
        // A library model must not land in the four-example cache: those slots
        // keep their legacy/textured meaning for smoke, bench and the controls.
        let pet = LoadSource::Library {
            id: "bangboo-eous".into(),
            path: None,
        };
        assert_eq!(pet.example_index(), None);
        assert_eq!(pet.label(), "Eous");
        assert_eq!(pet.library_id(), Some("bangboo-eous"));
        let custom = LoadSource::Library {
            id: "my-pet".into(),
            path: Some(PathBuf::from("custom.dmodel.json")),
        };
        assert_eq!(custom.label(), "my-pet");
        assert_eq!(LoadSource::Example2d.library_id(), None);
    }

    #[test]
    fn every_bundled_library_id_loads_its_own_asset() {
        for (id, _) in crate::library::BUILTIN {
            let source = LoadSource::Library {
                id: (*id).to_owned(),
                path: None,
            };
            let model = source.load(&LoadLimits::default()).unwrap();
            assert!(!model.name.is_empty(), "{id}");
        }
        // An id the table does not know fails closed instead of picking a
        // default asset.
        let unknown = LoadSource::Library {
            id: "not-a-bundled-model".into(),
            path: None,
        };
        assert!(unknown.load(&LoadLimits::default()).is_err());
    }

    #[test]
    fn file_source_still_rejects_network_paths_before_reading() {
        for path in [
            "//server/share/model.dmodel.json",
            r"\\server\share\model.dmodel.json",
        ] {
            let error = LoadSource::File(PathBuf::from(path))
                .load(&LoadLimits::default())
                .unwrap_err();
            assert!(error.contains("Only local model paths"));
            let error = LoadSource::Library {
                id: "remote".into(),
                path: Some(PathBuf::from(path)),
            }
            .load(&LoadLimits::default())
            .unwrap_err();
            assert!(error.contains("Only local model paths"));
        }
    }
}
