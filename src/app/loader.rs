use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

use crate::model::{CompiledModel, LoadLimits, load_model_bytes, read_model_file};

pub(super) const QUEUE_CAPACITY: usize = 4;

#[derive(Clone, Debug)]
pub(super) enum LoadSource {
    Example2d,
    Example3d,
    TexturedExample2d,
    TexturedExample3d,
    File(PathBuf),
    ValidationProbe,
}

impl LoadSource {
    pub fn label(&self) -> String {
        match self {
            Self::Example2d => "Bundled 2D example".into(),
            Self::Example3d => "Bundled 3D example".into(),
            Self::TexturedExample2d => "Bundled textured 2D example".into(),
            Self::TexturedExample3d => "Bundled textured 3D example".into(),
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
            Self::File(_) | Self::ValidationProbe => None,
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
    fn file_source_still_rejects_network_paths_before_reading() {
        for path in [
            "//server/share/model.dmodel.json",
            r"\\server\share\model.dmodel.json",
        ] {
            let error = LoadSource::File(PathBuf::from(path))
                .load(&LoadLimits::default())
                .unwrap_err();
            assert!(error.contains("Only local model paths"));
        }
    }
}
