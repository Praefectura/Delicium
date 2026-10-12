//! Asset pool and render-asset conversion.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::{
    model::{CompiledModel, ModelData},
    renderer::{LayerAsset2d, RenderAsset, SkinVertex},
};

use super::{AssetId, MAX_ASSETS, SimError};

/// Registry of compiled models, keyed by monotonically increasing ids.
///
/// Ids never depend on file names, so inserting two models that share a name
/// can never overwrite an entry. The map orders by id, which lets
/// [`AssetPool::collect_unused`] return ascending ids without extra sorting.
pub(crate) struct AssetPool {
    entries: BTreeMap<AssetId, Arc<CompiledModel>>,
    next_id: AssetId,
}

impl AssetPool {
    /// Creates an empty pool.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// Registers a model and returns its new id.
    pub fn insert(&mut self, model: Arc<CompiledModel>) -> Result<AssetId, SimError> {
        if self.entries.len() >= MAX_ASSETS {
            return Err(SimError::AssetLimit { limit: MAX_ASSETS });
        }
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(SimError::IdExhausted)?;
        self.entries.insert(id, model);
        Ok(id)
    }

    /// Looks up a registered asset.
    pub fn get(&self, id: AssetId) -> Option<&Arc<CompiledModel>> {
        self.entries.get(&id)
    }

    /// Number of registered assets.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Removes one asset if present, returning whether it was registered.
    pub fn remove(&mut self, id: AssetId) -> bool {
        self.entries.remove(&id).is_some()
    }

    /// Removes every asset that is neither in `used` nor in `keep` and
    /// returns the removed ids in ascending order.
    pub fn collect_unused(&mut self, used: &BTreeSet<AssetId>, keep: &[AssetId]) -> Vec<AssetId> {
        let keep: BTreeSet<AssetId> = keep.iter().copied().collect();
        let removed: Vec<AssetId> = self
            .entries
            .keys()
            .copied()
            .filter(|id| !used.contains(id) && !keep.contains(id))
            .collect();
        for id in &removed {
            self.entries.remove(id);
        }
        removed
    }
}

/// Converts a compiled model into the renderer's CPU-side asset description.
///
/// The result contains only geometry data (no GPU handles): layer masks become
/// point lists and 3D vertices become [`SkinVertex`] lanes, copying joint
/// indices and weights verbatim.
pub fn render_asset(model: &CompiledModel) -> RenderAsset {
    match &model.data {
        ModelData::Sprite2d(sprite) => RenderAsset::Sprite2d {
            layers: sprite
                .layers
                .iter()
                .map(|layer| LayerAsset2d {
                    vertices: layer.vertices.clone(),
                    indices: layer.indices.clone(),
                    color: layer.color,
                    mask: layer.mask.as_ref().map(|mask| mask.points.clone()),
                })
                .collect(),
        },
        ModelData::Skeleton3d(skeleton) => RenderAsset::Skeleton3d {
            vertices: skeleton
                .vertices
                .iter()
                .map(|vertex| SkinVertex {
                    position: vertex.position,
                    normal: vertex.normal,
                    color: vertex.color,
                    joints: vertex.joints,
                    weights: vertex.weights,
                })
                .collect(),
            indices: skeleton.indices.clone(),
            joint_count: skeleton.joints.len(),
        },
    }
}
