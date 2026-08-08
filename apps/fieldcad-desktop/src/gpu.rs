//! Graphics backend selection, and a windowless path for validating it.
//!
//! A driver or compositor problem on one machine must not be an unfixable wall.
//! Backend, present mode, and adapter class are all overridable from the
//! environment, and [`smoke_test`] exercises the whole GPU path — adapter,
//! device, shaders, pipelines, submission — without creating a window, so it can
//! be run safely when a windowed run is what is misbehaving.

use std::time::Duration;

/// How the graphics stack should be brought up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuConfig {
    pub backends: wgpu::Backends,
    /// `None` means "use the surface's preferred mode".
    pub present_mode: Option<wgpu::PresentMode>,
    pub force_fallback_adapter: bool,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            backends: wgpu::Backends::all().with_env(),
            present_mode: None,
            force_fallback_adapter: false,
        }
    }
}

impl GpuConfig {
    /// Read overrides from the environment.
    ///
    /// - `WGPU_BACKEND` — `vulkan`, `gl`, `metal`, `dx12`, or a comma-separated
    ///   list. Interpreted by `wgpu` itself.
    /// - `FIELDCAD_PRESENT_MODE` — `vsync`, `no-vsync`, `fifo`, `fifo-relaxed`,
    ///   `mailbox`, `immediate`.
    /// - `FIELDCAD_FORCE_FALLBACK` — `1` to demand a software adapter.
    pub fn from_env() -> Self {
        let present_mode = std::env::var("FIELDCAD_PRESENT_MODE")
            .ok()
            .and_then(|value| {
                let parsed = parse_present_mode(&value);
                if parsed.is_none() {
                    tracing::warn!(
                        value = %value,
                        "unrecognised FIELDCAD_PRESENT_MODE; using the surface default"
                    );
                }
                parsed
            });

        let force_fallback_adapter = std::env::var("FIELDCAD_FORCE_FALLBACK")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));

        Self {
            backends: wgpu::Backends::all().with_env(),
            present_mode,
            force_fallback_adapter,
        }
    }

    pub fn instance(&self) -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: self.backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        })
    }
}

fn parse_present_mode(value: &str) -> Option<wgpu::PresentMode> {
    match value.trim().to_lowercase().replace('_', "-").as_str() {
        "vsync" | "auto-vsync" => Some(wgpu::PresentMode::AutoVsync),
        "no-vsync" | "auto-no-vsync" | "novsync" => Some(wgpu::PresentMode::AutoNoVsync),
        "fifo" => Some(wgpu::PresentMode::Fifo),
        "fifo-relaxed" => Some(wgpu::PresentMode::FifoRelaxed),
        "mailbox" => Some(wgpu::PresentMode::Mailbox),
        "immediate" => Some(wgpu::PresentMode::Immediate),
        _ => None,
    }
}

/// Choose a present mode the surface actually supports.
///
/// An unsupported mode is a validation error on some backends and silently
/// ignored on others, so a requested mode that is not offered is reported and
/// the surface default is used instead.
pub fn choose_present_mode(
    requested: Option<wgpu::PresentMode>,
    supported: &[wgpu::PresentMode],
    default: wgpu::PresentMode,
) -> wgpu::PresentMode {
    let Some(requested) = requested else {
        return default;
    };
    // The `Auto*` modes are resolved by wgpu against what the surface offers,
    // so they are always legal to request.
    let always_available = matches!(
        requested,
        wgpu::PresentMode::AutoVsync | wgpu::PresentMode::AutoNoVsync
    );
    if always_available || supported.contains(&requested) {
        requested
    } else {
        tracing::warn!(
            ?requested,
            ?supported,
            "requested present mode is not supported by this surface; using the default"
        );
        default
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SmokeTestError {
    #[error("no graphics adapter was available: {0}")]
    NoAdapter(wgpu::RequestAdapterError),
    #[error("could not create a graphics device: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),
    #[error("the device did not complete submitted work: {0}")]
    Poll(#[from] wgpu::PollError),
}

/// What a windowless render actually managed to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmokeTestReport {
    pub adapter: String,
    pub backend: String,
    pub device_type: String,
    pub frames: u32,
}

/// Render the scene offscreen, with no window and no compositor involvement.
///
/// This is the safe way to answer "is this backend usable on this machine?".
/// A windowed run can wedge a compositor; this cannot, because it never creates
/// a surface.
pub async fn smoke_test(config: GpuConfig, frames: u32) -> Result<SmokeTestReport, SmokeTestError> {
    const SIZE: u32 = 256;
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    let instance = config.instance();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: config.force_fallback_adapter,
            compatible_surface: None,
        })
        .await
        .map_err(SmokeTestError::NoAdapter)?;

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("Field CAD smoke-test device"),
            ..Default::default()
        })
        .await?;

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Smoke-test colour target"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    // The same pipelines, shader, and draw calls the window path uses.
    let mut scene = crate::renderer::SceneRenderer::new(&device, FORMAT);
    let depth = crate::renderer::depth_view(&device, SIZE, SIZE);
    let camera = crate::camera::OrbitCamera::default();
    let instances = [crate::scene::ObjectInstance {
        id: fieldcad_core::ObjectId::new(0),
        model: glam::Mat4::IDENTITY,
        half_extent: glam::Vec3::ONE,
        mesh: crate::scene::ObjectMesh::Sphere,
        selected: false,
    }];
    let field = crate::scene::FieldGeometry::default();

    for _ in 0..frames {
        scene.update(
            &device,
            &queue,
            &camera,
            1.0,
            &instances,
            &field,
            0.0,
            [SIZE as f32, SIZE as f32],
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Smoke-test encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Smoke-test pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            scene.draw(&mut pass, true, true);
        }
        queue.submit(std::iter::once(encoder.finish()));
        device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })?;
    }

    let info = adapter.get_info();
    Ok(SmokeTestReport {
        adapter: info.name,
        backend: format!("{:?}", info.backend),
        device_type: format!("{:?}", info.device_type),
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_modes_are_parsed_case_and_separator_insensitively() {
        assert_eq!(
            parse_present_mode("Vsync"),
            Some(wgpu::PresentMode::AutoVsync)
        );
        assert_eq!(
            parse_present_mode("no_vsync"),
            Some(wgpu::PresentMode::AutoNoVsync)
        );
        assert_eq!(
            parse_present_mode(" immediate "),
            Some(wgpu::PresentMode::Immediate)
        );
        assert_eq!(parse_present_mode("nonsense"), None);
    }

    #[test]
    fn an_unsupported_present_mode_falls_back_instead_of_being_submitted() {
        let supported = [wgpu::PresentMode::Fifo];

        assert_eq!(
            choose_present_mode(
                Some(wgpu::PresentMode::Mailbox),
                &supported,
                wgpu::PresentMode::Fifo
            ),
            wgpu::PresentMode::Fifo
        );
        assert_eq!(
            choose_present_mode(
                Some(wgpu::PresentMode::Immediate),
                &supported,
                wgpu::PresentMode::Fifo
            ),
            wgpu::PresentMode::Fifo
        );
    }

    #[test]
    fn a_supported_or_auto_present_mode_is_honoured() {
        let supported = [wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox];

        assert_eq!(
            choose_present_mode(
                Some(wgpu::PresentMode::Mailbox),
                &supported,
                wgpu::PresentMode::Fifo
            ),
            wgpu::PresentMode::Mailbox
        );
        // `Auto*` is resolved by wgpu, so it need not appear in `supported`.
        assert_eq!(
            choose_present_mode(
                Some(wgpu::PresentMode::AutoNoVsync),
                &supported,
                wgpu::PresentMode::Fifo
            ),
            wgpu::PresentMode::AutoNoVsync
        );
    }

    #[test]
    fn no_request_uses_the_surface_default() {
        assert_eq!(
            choose_present_mode(None, &[], wgpu::PresentMode::Fifo),
            wgpu::PresentMode::Fifo
        );
    }

    /// Renders offscreen on whatever adapter exists. Skips rather than fails
    /// where there is none, so it is safe in CI.
    #[test]
    fn the_scene_renders_offscreen_without_a_window() {
        let config = GpuConfig::default();
        match pollster::block_on(smoke_test(config, 2)) {
            Ok(report) => {
                assert_eq!(report.frames, 2);
                assert!(!report.adapter.is_empty());
            }
            Err(SmokeTestError::NoAdapter(_)) => {
                eprintln!("skipping: no graphics adapter available");
            }
            Err(error) => panic!("offscreen render failed: {error}"),
        }
    }
}
