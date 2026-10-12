//! Windowed front end.
//!
//! [`Graphics`] owns the GPU instance, one adapter/device/queue pair, two
//! surfaces (the transparent overlay window and the opaque control window), a
//! [`SceneRenderer`] used for the overlay and an independent egui renderer used
//! for the control window.
//!
//! Windows transparency: the only real DX12 transparency path is
//! DirectComposition (`Dx12SwapchainKind::DxgiFromVisual`), which is forced
//! here; a surface created that way advertises `CompositeAlphaMode::PreMultiplied`.
//! If the advertised alpha modes do not include `PreMultiplied`, construction
//! fails with [`RenderError::TransparencyUnavailable`] instead of silently
//! falling back to an opaque window; the actual mode in use is recorded and
//! queryable through [`Graphics::transparency_mode`].

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use winit::{
    dpi::PhysicalSize,
    window::{Window, WindowId},
};

use super::error::RenderError;
use super::scene::SceneRenderer;
use super::types::{BackendChoice, RenderAsset, RenderFrame};

/// The windowed graphics front end.
pub struct Graphics {
    #[allow(dead_code)]
    instance: wgpu::Instance,
    #[allow(dead_code)]
    adapter: wgpu::Adapter,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,

    #[allow(dead_code)]
    overlay: Arc<Window>,
    #[allow(dead_code)]
    control: Arc<Window>,
    overlay_id: WindowId,
    control_id: WindowId,

    overlay_surface: wgpu::Surface<'static>,
    control_surface: wgpu::Surface<'static>,
    overlay_config: wgpu::SurfaceConfiguration,
    control_config: wgpu::SurfaceConfiguration,

    scene: SceneRenderer,
    egui_renderer: egui_wgpu::Renderer,

    transparency: wgpu::CompositeAlphaMode,
    adapter_description: String,
    device_lost: Arc<AtomicBool>,
}

impl Graphics {
    /// Creates the device and both window surfaces.
    ///
    /// Fails with [`RenderError::TransparencyUnavailable`] when the overlay
    /// surface cannot actually be composited with transparency
    /// (`PreMultiplied` not among its alpha modes). No opaque fallback is
    /// claimed.
    pub async fn new(
        overlay: Arc<Window>,
        control: Arc<Window>,
        backend: BackendChoice,
    ) -> Result<Self, RenderError> {
        let backends = match backend {
            // Windows overlay transparency requires DX12 DirectComposition.
            BackendChoice::Auto => wgpu::Backends::DX12,
            BackendChoice::Dx12 => wgpu::Backends::DX12,
            BackendChoice::Vulkan => wgpu::Backends::VULKAN,
        };
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = backends;
        // DirectComposition on DX12 is the only real Windows transparency path.
        descriptor.backend_options.dx12.presentation_system =
            wgpu::Dx12SwapchainKind::DxgiFromVisual;
        let instance = wgpu::Instance::new(descriptor);

        let overlay_surface = instance.create_surface(overlay.clone())?;
        let control_surface = instance.create_surface(control.clone())?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&overlay_surface),
                apply_limit_buckets: false,
                ..Default::default()
            })
            .await
            .map_err(|error| RenderError::NoSuitableAdapter {
                details: error.to_string(),
            })?;

        if !adapter.is_surface_supported(&control_surface) {
            return Err(RenderError::SurfaceUnsupported {
                details: "the selected adapter cannot present to the control window".into(),
            });
        }

        let overlay_caps = overlay_surface.get_capabilities(&adapter);
        if overlay_caps.formats.is_empty() {
            return Err(RenderError::SurfaceUnsupported {
                details: "overlay surface advertises no texture formats".into(),
            });
        }
        let transparency = overlay_caps
            .alpha_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::CompositeAlphaMode::PreMultiplied)
            .ok_or_else(|| RenderError::TransparencyUnavailable {
                details: format!(
                    "overlay surface alpha modes are {:?}; PreMultiplied is not available, so real transparency cannot be provided",
                    overlay_caps.alpha_modes
                ),
            })?;

        let overlay_format = overlay_caps.formats[0];
        let overlay_present_mode = pick_present_mode(&overlay_caps.present_modes)?;
        let overlay_size = overlay.inner_size();
        let overlay_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: overlay_format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: overlay_size.width.max(1),
            height: overlay_size.height.max(1),
            present_mode: overlay_present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: transparency,
            view_formats: vec![],
        };

        let control_caps = control_surface.get_capabilities(&adapter);
        if control_caps.formats.is_empty() {
            return Err(RenderError::SurfaceUnsupported {
                details: "control surface advertises no texture formats".into(),
            });
        }
        let control_format = control_caps.formats[0];
        let control_alpha = control_caps
            .alpha_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::CompositeAlphaMode::Opaque)
            .or_else(|| control_caps.alpha_modes.first().copied())
            .ok_or_else(|| RenderError::SurfaceUnsupported {
                details: "control surface advertises no alpha modes".into(),
            })?;
        let control_present_mode = pick_present_mode(&control_caps.present_modes)?;
        let control_size = control.inner_size();
        let control_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: control_format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: control_size.width.max(1),
            height: control_size.height.max(1),
            present_mode: control_present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: control_alpha,
            view_formats: vec![],
        };

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Delicium device"),
                ..Default::default()
            })
            .await
            .map_err(|error| RenderError::DeviceRequest {
                details: error.to_string(),
            })?;
        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let device_lost = Arc::new(AtomicBool::new(false));
        {
            let flag = device_lost.clone();
            device.set_device_lost_callback(move |_reason, message| {
                log::error!("GPU device lost: {message}");
                flag.store(true, Ordering::SeqCst);
            });
        }

        overlay_surface.configure(&device, &overlay_config);
        control_surface.configure(&device, &control_config);

        let scene = SceneRenderer::new(device.clone(), queue.clone(), overlay_format)?;
        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            control_format,
            egui_wgpu::RendererOptions::default(),
        );

        let info = adapter.get_info();
        let adapter_description = format!(
            "{} ({:?}, {:?}; driver {})",
            info.name, info.backend, info.device_type, info.driver
        );

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            overlay_id: overlay.id(),
            control_id: control.id(),
            overlay,
            control,
            overlay_surface,
            control_surface,
            overlay_config,
            control_config,
            scene,
            egui_renderer,
            transparency,
            adapter_description,
            device_lost,
        })
    }

    /// A human-readable description of the adapter in use.
    pub fn adapter_description(&self) -> &str {
        &self.adapter_description
    }

    /// The alpha mode actually used for the overlay surface.
    pub fn transparency_mode(&self) -> wgpu::CompositeAlphaMode {
        self.transparency
    }

    /// The device, for tests that need to drive the renderer directly.
    pub fn device(&self) -> Arc<wgpu::Device> {
        self.device.clone()
    }

    /// The queue, for tests that need to drive the renderer directly.
    pub fn queue(&self) -> Arc<wgpu::Queue> {
        self.queue.clone()
    }

    /// Registers (or replaces) one asset with the scene renderer.
    pub fn register_asset(&mut self, id: u64, asset: &RenderAsset) -> Result<(), RenderError> {
        self.scene.register_asset(id, asset)
    }

    /// Removes an asset from the scene renderer.
    pub fn remove_asset(&mut self, id: u64) {
        self.scene.remove_asset(id);
    }

    /// Draws one scene frame on the transparent overlay window.
    ///
    /// Frames the surface cannot provide right now (timeout, occluded) are
    /// skipped quietly; lost or outdated surfaces are reconfigured and the
    /// frame is skipped. Only conditions the caller must know about (device
    /// loss, out of memory, invalid frame data) are returned as errors.
    pub fn render_scene(&mut self, frame: &RenderFrame) -> Result<(), RenderError> {
        if self.overlay_config.width == 0 || self.overlay_config.height == 0 {
            return Ok(());
        }
        let Some(texture) = self.acquire_overlay()? else {
            return Ok(());
        };

        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Delicium scene frame"),
            });
        match self.scene.render_to(
            frame,
            &view,
            [self.overlay_config.width, self.overlay_config.height],
            &mut encoder,
        ) {
            Ok(()) => {}
            Err(error) => {
                drop(encoder);
                return Err(error);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(texture);
        Ok(())
    }

    /// Draws the egui control layer on the opaque control window.
    pub fn render_control(
        &mut self,
        jobs: &[egui::ClippedPrimitive],
        textures: &egui::TexturesDelta,
        pixels_per_point: f32,
    ) -> Result<(), RenderError> {
        // Apply texture deltas even when the window cannot draw this frame.
        for (id, image_deltas) in &textures.set {
            for image_delta in image_deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, *id, image_delta);
            }
        }

        let texture = if self.control_config.width == 0 || self.control_config.height == 0 {
            None
        } else {
            self.acquire_control()?
        };
        let Some(texture) = texture else {
            // egui does not resend frees from a skipped frame.
            for id in &textures.free {
                self.egui_renderer.free_texture(id);
            }
            return Ok(());
        };

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.control_config.width, self.control_config.height],
            pixels_per_point,
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Delicium control frame"),
            });
        let user_command_buffers = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            jobs,
            &screen_descriptor,
        );
        {
            let view = texture
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Delicium control pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.egui_renderer
                .render(&mut pass.forget_lifetime(), jobs, &screen_descriptor);
        }
        self.queue.submit(
            user_command_buffers
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );

        // Free after submission: the GPU may still be reading these textures.
        for id in &textures.free {
            self.egui_renderer.free_texture(id);
        }

        self.queue.present(texture);
        Ok(())
    }

    /// Resizes one of the two windows' surfaces.
    ///
    /// A zero-sized (minimized) window is left configured as-is; the current
    /// size is restored when a nonzero size arrives.
    pub fn resize(&mut self, id: WindowId, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        if id == self.overlay_id {
            self.overlay_config.width = size.width;
            self.overlay_config.height = size.height;
            self.overlay_surface
                .configure(&self.device, &self.overlay_config);
        } else if id == self.control_id {
            self.control_config.width = size.width;
            self.control_config.height = size.height;
            self.control_surface
                .configure(&self.device, &self.control_config);
        }
    }

    // -- surface acquisition -------------------------------------------------

    /// Acquires the next overlay texture, transparently recovering from
    /// skippable and recoverable surface states. Returns `Ok(None)` when the
    /// frame should be skipped.
    fn acquire_overlay(&mut self) -> Result<Option<wgpu::SurfaceTexture>, RenderError> {
        let outcome = self.overlay_surface.get_current_texture();
        match self.handle_surface_state(outcome, true)? {
            Some(texture) => Ok(Some(texture)),
            None => Ok(None),
        }
    }

    /// Acquires the next control texture, same state handling as the overlay.
    fn acquire_control(&mut self) -> Result<Option<wgpu::SurfaceTexture>, RenderError> {
        let outcome = self.control_surface.get_current_texture();
        match self.handle_surface_state(outcome, false)? {
            Some(texture) => Ok(Some(texture)),
            None => Ok(None),
        }
    }

    /// Shared surface state machine for both windows.
    ///
    /// - `Success` -> use the texture.
    /// - `Suboptimal` -> reconfigure now, then use the texture.
    /// - `Timeout` / `Occluded` -> skip the frame (`Ok(None)`); not an error.
    /// - `Outdated` -> reconfigure and skip this frame.
    /// - `Lost` -> a lost device is fatal; otherwise recreate the surface,
    ///   reconfigure and skip this frame.
    /// - `Validation` -> a real error surfaced to the caller.
    fn handle_surface_state(
        &mut self,
        outcome: wgpu::CurrentSurfaceTexture,
        overlay: bool,
    ) -> Result<Option<wgpu::SurfaceTexture>, RenderError> {
        match outcome {
            wgpu::CurrentSurfaceTexture::Success(texture) => Ok(Some(texture)),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                // Reconfiguring while this frame's texture is alive panics
                // (Surface::configure is #[should_panic] in that case), and the
                // texture is suboptimal anyway: drop it, reconfigure, and draw
                // on the next frame.
                drop(texture);
                if overlay {
                    self.overlay_surface
                        .configure(&self.device, &self.overlay_config);
                } else {
                    self.control_surface
                        .configure(&self.device, &self.control_config);
                }
                Ok(None)
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                log::debug!("surface acquire timed out; frame skipped");
                Ok(None)
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                log::debug!("window occluded; frame skipped");
                Ok(None)
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                if overlay {
                    self.overlay_surface
                        .configure(&self.device, &self.overlay_config);
                } else {
                    self.control_surface
                        .configure(&self.device, &self.control_config);
                }
                Ok(None)
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                if self.device_lost.load(Ordering::SeqCst) {
                    return Err(RenderError::DeviceLost {
                        details: "surface lost after device loss".into(),
                    });
                }
                // A lost surface must be recreated, reconfiguring alone does
                // not recover it.
                if overlay {
                    let surface =
                        self.instance
                            .create_surface(self.overlay.clone())
                            .map_err(|error| RenderError::SurfaceUnsupported {
                                details: format!("cannot recreate overlay surface: {error}"),
                            })?;
                    surface.configure(&self.device, &self.overlay_config);
                    self.overlay_surface = surface;
                } else {
                    let surface =
                        self.instance
                            .create_surface(self.control.clone())
                            .map_err(|error| RenderError::SurfaceUnsupported {
                                details: format!("cannot recreate control surface: {error}"),
                            })?;
                    surface.configure(&self.device, &self.control_config);
                    self.control_surface = surface;
                }
                Ok(None)
            }
            wgpu::CurrentSurfaceTexture::Validation => Err(RenderError::SurfaceOutdated {
                details: "surface acquisition hit a validation error; reconfiguration required"
                    .into(),
            }),
        }
    }
}

fn pick_present_mode(modes: &[wgpu::PresentMode]) -> Result<wgpu::PresentMode, RenderError> {
    if modes.contains(&wgpu::PresentMode::Fifo) {
        Ok(wgpu::PresentMode::Fifo)
    } else {
        modes
            .first()
            .copied()
            .ok_or_else(|| RenderError::SurfaceUnsupported {
                details: "surface advertises no presentation modes".into(),
            })
    }
}
