use glam::Mat4;

pub type RenderAssetId = u64;
pub type RenderInstanceId = u64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendChoice {
    #[default]
    Auto,
    Dx12,
    Vulkan,
}

#[derive(Clone, Debug)]
pub struct LayerAsset2d {
    pub vertices: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub color: [f32; 4],
    pub mask: Option<Vec<[f32; 2]>>,
}

#[derive(Clone, Copy, Debug)]
pub struct SkinVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4],
    pub joints: [u32; 4],
    pub weights: [f32; 4],
}

#[derive(Clone, Debug)]
pub enum RenderAsset {
    Sprite2d {
        layers: Vec<LayerAsset2d>,
    },
    Skeleton3d {
        vertices: Vec<SkinVertex>,
        indices: Vec<u32>,
        joint_count: usize,
    },
}

#[derive(Clone, Debug)]
pub enum InstanceDraw {
    Sprite2d {
        asset: RenderAssetId,
        instance: RenderInstanceId,
        transform: Mat4,
        layers: Vec<Vec<[f32; 2]>>,
    },
    Skeleton3d {
        asset: RenderAssetId,
        instance: RenderInstanceId,
        transform: Mat4,
        skin_matrices: Vec<Mat4>,
    },
}

impl InstanceDraw {
    pub fn instance(&self) -> RenderInstanceId {
        match self {
            Self::Sprite2d { instance, .. } | Self::Skeleton3d { instance, .. } => *instance,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RenderFrame {
    pub instances: Vec<InstanceDraw>,
}

pub fn scene_projection(width: u32, height: u32) -> Mat4 {
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    let half_height = 2.25;
    let half_width = half_height * aspect;
    glam::camera::rh::proj::directx::orthographic(-half_width, half_width, -0.9, 3.6, 0.1, 100.0)
        * glam::camera::rh::view::look_at_mat4(
            glam::Vec3::new(0.0, 0.0, 10.0),
            glam::Vec3::ZERO,
            glam::Vec3::Y,
        )
}

pub fn screen_to_world(cursor: [f32; 2], size: [u32; 2]) -> [f32; 2] {
    let width = size[0].max(1) as f32;
    let height = size[1].max(1) as f32;
    let half_width = 2.25 * width / height;
    [
        (cursor[0] / width * 2.0 - 1.0) * half_width,
        3.6 - cursor[1] / height * 4.5,
    ]
}
