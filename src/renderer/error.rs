//! Renderer error type shared by the offscreen scene renderer and the windowed graphics layer.

use std::fmt;

/// Errors produced while creating or driving the renderer.
///
/// The variants intentionally carry enough detail for the application to decide whether a
/// failure is recoverable (reconfigure and retry next frame), skippable (timeout), or fatal
/// (device lost / out of memory), without inventing success.
#[derive(Debug)]
pub enum RenderError {
    /// No GPU adapter matched the requested backend or could present to the surface.
    NoSuitableAdapter { details: String },
    /// A surface exists but its capabilities are not usable for the requested purpose.
    SurfaceUnsupported { details: String },
    /// The requested platform transparency is not available; no opaque fallback is claimed.
    TransparencyUnavailable { details: String },
    /// Failed to create a wgpu device or queue.
    DeviceRequest { details: String },
    /// Asset data is malformed or violates renderer budgets.
    InvalidAsset { asset: u64, details: String },
    /// Frame data is malformed (layer count, skin matrices, or non-finite values).
    InvalidFrame { instance: u64, details: String },
    /// A GPU resource creation was rejected while inside an error scope.
    GpuResource { details: String },
    /// The surface is lost/outdated; the caller should reconfigure and retry on a later frame.
    SurfaceOutdated { details: String },
    /// The swap chain could not provide an image this frame (timeout/occluded); skip the frame.
    SurfaceTimeout { details: String },
    /// The device was lost; rendering cannot continue on this device.
    DeviceLost { details: String },
    /// Out of GPU memory.
    OutOfMemory { details: String },
    /// The requested identifier is not registered.
    UnknownAsset { asset: u64 },
    /// A window identifier does not belong to this graphics instance.
    UnknownWindow,
    /// Registering or rendering with this format would need an incompatible pipeline.
    Unsupported { details: String },
    /// Internal inconsistency that should not be reachable.
    Internal { details: String },
}

impl RenderError {
    /// True when the failure is tied to the swap chain state and a reconfigure can recover it.
    pub fn is_recoverable_surface(&self) -> bool {
        matches!(
            self,
            Self::SurfaceOutdated { .. } | Self::SurfaceTimeout { .. }
        )
    }

    /// True when the frame can simply be skipped without changing any configuration.
    pub fn is_skippable_frame(&self) -> bool {
        matches!(self, Self::SurfaceTimeout { .. })
    }

    /// True when the GPU device itself is unusable; no retry can succeed on this device.
    pub fn is_fatal_device(&self) -> bool {
        matches!(self, Self::DeviceLost { .. } | Self::OutOfMemory { .. })
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuitableAdapter { details } => write!(f, "No suitable GPU adapter: {details}"),
            Self::SurfaceUnsupported { details } => {
                write!(f, "Surface not usable by the selected adapter: {details}")
            }
            Self::TransparencyUnavailable { details } => {
                write!(f, "Transparent compositing is unavailable: {details}")
            }
            Self::DeviceRequest { details } => write!(f, "Cannot create GPU device: {details}"),
            Self::InvalidAsset { asset, details } => {
                write!(f, "Asset {asset} rejected by the renderer: {details}")
            }
            Self::InvalidFrame { instance, details } => {
                write!(f, "Instance {instance} frame data rejected: {details}")
            }
            Self::GpuResource { details } => write!(f, "GPU resource creation failed: {details}"),
            Self::SurfaceOutdated { details } => {
                write!(
                    f,
                    "Surface is outdated or lost, reconfigure required: {details}"
                )
            }
            Self::SurfaceTimeout { details } => {
                write!(f, "Surface did not provide a frame (skipped): {details}")
            }
            Self::DeviceLost { details } => write!(f, "GPU device lost: {details}"),
            Self::OutOfMemory { details } => write!(f, "GPU out of memory: {details}"),
            Self::UnknownAsset { asset } => write!(f, "Asset {asset} is not registered"),
            Self::UnknownWindow => write!(f, "The window does not belong to this renderer"),
            Self::Unsupported { details } => {
                write!(f, "Unsupported renderer configuration: {details}")
            }
            Self::Internal { details } => write!(f, "Internal renderer error: {details}"),
        }
    }
}

impl std::error::Error for RenderError {}

impl From<wgpu::RequestDeviceError> for RenderError {
    fn from(error: wgpu::RequestDeviceError) -> Self {
        Self::DeviceRequest {
            details: error.to_string(),
        }
    }
}

impl From<wgpu::CreateSurfaceError> for RenderError {
    fn from(error: wgpu::CreateSurfaceError) -> Self {
        Self::SurfaceUnsupported {
            details: error.to_string(),
        }
    }
}
