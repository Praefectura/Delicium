//! Rendering layer: frozen frame types, CPU mask rasterization, the offscreen
//! [`SceneRenderer`] and the windowed [`Graphics`] front end.
//!
//! [`types`] is the frozen producer/renderer interface (simulation writes
//! [`RenderFrame`]s, the renderer consumes them). Everything else in this
//! module tree is free to evolve behind the [`SceneRenderer`] and [`Graphics`]
//! entry points.

pub mod error;
pub mod graphics;
mod mask;
pub mod scene;
mod types;

pub use error::*;
pub use graphics::Graphics;
pub use scene::SceneRenderer;
pub use types::*;
