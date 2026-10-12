//! Native Delicium model pipeline: `.dmodel.json` -> validated -> compiled.
//!
//! This module is self-contained: no file paths, URLs, scripts or executable
//! content inside a model, no window/GPU/ECS dependency, and no panics on
//! untrusted input. It is usable from a headless test binary.
//!
//! ```text
//! bytes -> schema::RawEnvelope -> validate -> compile -> Arc<CompiledModel>
//!                                                             |
//!                                    simulation / renderer read
//! ```
//!
//! # Entry points
//!
//! - [`load_model_bytes`]: parse, validate and compile an in-memory payload.
//! - [`read_model_file`]: read a file with a hard byte budget, then compile.
//! - [`sample_parameters`], [`deform_layer`]: 2D evaluation.
//! - [`sample_joints`], [`skin_matrices`]: 3D evaluation.
//!
//! # Format
//!
//! `format` must be exactly `delicium.model`, `format_version` exactly `0.1`
//! and `kind` either `sprite2d` or `skeleton3d`. Unknown fields anywhere in the
//! document are rejected (`deny_unknown_fields`), so a newer file cannot be
//! half-loaded and mistaken for a supported one.
//!
//! # Guarantees of a compiled asset
//!
//! - every name is unique in its namespace and at most 128 characters;
//! - joints and deformers are reordered so that `parent < index`, making a
//!   single forward pass sufficient for hierarchies;
//! - quaternions and normals are unit-length, skin weights are non-negative
//!   and sum to `1.0`, and every zero-weight lane points at a real joint;
//! - `inverse_bind` is indexed like `joints` and is finite and invertible;
//! - all values are finite and inside the documented bounds;
//! - grid meshes, warp control grids and animation keys are inside the
//!   configured budgets.
//!
//! Input that violates any of this is rejected with a [`ModelError`]; the
//! source file is never modified.

mod compile;
mod error;
mod evaluate;
mod limits;
mod schema;
mod types;
mod validate;

pub use error::ModelError;
pub use evaluate::{
    deform_layer, sample_joints, sample_parameters, skin_matrices, try_deform_layer,
    try_sample_joints, try_sample_parameters, try_skin_matrices,
};
pub use limits::LoadLimits;
pub use types::{
    AnimationInterpolation, CompiledModel, CompiledSkeleton3d, CompiledSprite2d, Deformer2d,
    InteractionBinding, Joint, JointTransform, Layer2d, Mask2d, ModelData, ModelKind, ParamChannel,
    Parameter, ParameterAnimation, RotationKey, ScalarKey, ScaleKey, SkeletalAnimation,
    SkeletalChannel, TranslationKey, Vertex3d,
};

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

/// Maximum length of any name in a document. Also enforced by the validator;
/// kept here as documentation of the format contract.
pub const MAX_NAME_LEN: usize = 128;

/// Parses, validates and compiles a model from an in-memory payload.
///
/// The payload length is checked against [`LoadLimits::max_file_bytes`] before
/// parsing, so an in-memory caller cannot bypass the file budget.
pub fn load_model_bytes(
    bytes: &[u8],
    limits: &LoadLimits,
) -> Result<Arc<CompiledModel>, ModelError> {
    if bytes.len() > limits.max_file_bytes {
        return Err(ModelError::FileTooLarge {
            actual: bytes.len() as u64,
            max: limits.max_file_bytes,
        });
    }

    let raw: schema::RawEnvelope =
        serde_json::from_slice(bytes).map_err(|err| ModelError::InvalidJson {
            message: err.to_string(),
        })?;

    validate::validate(&raw, limits)?;
    Ok(Arc::new(compile::compile(&raw, limits)?))
}

/// Reads a model file and compiles it.
///
/// The file is read through `File::take(max_file_bytes + 1)` and the *actual*
/// byte count is enforced, so metadata that lies (or a file that grows between
/// the metadata query and the read) cannot exceed the budget. Metadata is only
/// consulted to reject early.
pub fn read_model_file(path: &Path, limits: &LoadLimits) -> Result<Arc<CompiledModel>, ModelError> {
    let io_err = |source: std::io::Error| ModelError::Io {
        path: path.to_path_buf(),
        source,
    };

    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > limits.max_file_bytes as u64
    {
        return Err(ModelError::FileTooLarge {
            actual: meta.len(),
            max: limits.max_file_bytes,
        });
    }

    let file = std::fs::File::open(path).map_err(io_err)?;
    let budget = limits.max_file_bytes.saturating_add(1) as u64;

    let mut bytes = Vec::new();
    file.take(budget).read_to_end(&mut bytes).map_err(io_err)?;

    if bytes.len() > limits.max_file_bytes {
        return Err(ModelError::FileTooLarge {
            actual: bytes.len() as u64,
            max: limits.max_file_bytes,
        });
    }

    load_model_bytes(&bytes, limits)
}
