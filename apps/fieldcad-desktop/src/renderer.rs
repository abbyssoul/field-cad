use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

use crate::{
    camera::{OrbitCamera, Viewport},
    scene::{ColoredVertex, FieldGeometry, FlowRibbonVertex, ObjectInstance, ObjectMesh},
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.025,
    g: 0.035,
    b: 0.055,
    a: 1.0,
};
/// `pub(crate)`, not private: the object inspector's color picker shows this
/// as the live preview for an object whose `color` is unset, so a user sees
/// exactly what "unset" looks like rather than a UI-invented placeholder.
pub(crate) const OBJECT_COLOR: [f32; 4] = [0.2, 0.56, 0.88, 1.0];
const SELECTED_COLOR: [f32; 4] = [1.0, 0.48, 0.08, 1.0];

/// The shader source, exposed so a test can compile it without a GPU.
pub const SCENE_SHADER: &str = include_str!("scene.wgsl");

#[derive(Debug, thiserror::Error)]
pub(crate) enum RendererInitError {
    #[error("could not create a presentation surface: {0}")]
    CreateSurface(#[from] wgpu::CreateSurfaceError),
    #[error(
        "no usable graphics adapter was found, including software fallback. \
         Install a Vulkan, Metal, or Direct3D 12 driver, or a software renderer \
         such as Mesa lavapipe. Underlying error: {0}"
    )]
    NoAdapter(wgpu::RequestAdapterError),
    #[error("could not create a graphics device: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),
    #[error("the selected adapter cannot present to this window")]
    UnsupportedSurface,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderStatus {
    Presented,
    Skipped,
    /// The window cannot present — minimized or fully covered. The caller should
    /// stop asking at frame rate until this changes.
    Occluded,
    SurfaceLost,
}

pub(crate) struct GuiPaint<'a> {
    pub primitives: &'a [egui::ClippedPrimitive],
    pub textures_delta: &'a egui::TexturesDelta,
    pub pixels_per_point: f32,
}

/// What the renderer needs to draw one frame of the scene.
pub(crate) struct SceneFrame<'a> {
    pub camera: &'a OrbitCamera,
    pub viewport: Viewport,
    pub grid_visible: bool,
    pub axes_visible: bool,
    pub instances: &'a [ObjectInstance],
    /// The channel-layer geometry cached across frames — see
    /// `WindowState::field_layer_geometry`.
    pub base: &'a FieldGeometry,
    /// Live drag/selection-dependent geometry (authoring proxies, compute
    /// bounds, pending-edit ghosts, the transform gizmo) rebuilt fresh every
    /// frame, kept separate from `base` so a cache hit there is never
    /// mutated in place.
    pub overlay: &'a FieldGeometry,
    /// Wall-clock seconds since the window was created, independent of
    /// simulation time. Drives the animated flow-line shader; meaningless to
    /// everything else this renderer draws.
    pub time_seconds: f32,
}

/// Field order here is drop order, and it is load-bearing.
///
/// GPU resources must be released before the queue and device that own them;
/// the device before the surface; the surface before the adapter and instance;
/// and the surface owns the `Arc<Window>`, so the native window outlives every
/// object that refers to it. Declaring these in the intuitive
/// "instance, surface, device" order tears the stack down inside out and
/// segfaults on exit.
pub(crate) struct ViewportRenderer {
    gui: egui_wgpu::Renderer,
    scene: SceneRenderer,
    depth: DepthTarget,
    queue: wgpu::Queue,
    device: wgpu::Device,
    /// `Option` so that surface recreation can drop the old surface *before*
    /// creating its replacement. Two live surfaces for one window is not valid
    /// on every backend. Only `None` transiently during that swap.
    surface: Option<wgpu::Surface<'static>>,
    adapter: wgpu::Adapter,
    instance: wgpu::Instance,
    surface_config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    adapter_name: String,
    needs_reconfigure: bool,
}

impl Drop for ViewportRenderer {
    fn drop(&mut self) {
        // Let in-flight submissions retire before the device and its resources
        // are torn down underneath them.
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(5)),
        });
    }
}

impl ViewportRenderer {
    pub async fn new(
        window: Arc<Window>,
        config: crate::gpu::GpuConfig,
    ) -> Result<Self, RendererInitError> {
        let size = window.inner_size();
        let instance = config.instance();
        let surface = instance.create_surface(window)?;
        let adapter = request_adapter(&instance, &surface, config).await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Field CAD graphics device"),
                ..Default::default()
            })
            .await?;

        let mut surface_config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or(RendererInitError::UnsupportedSurface)?;
        let capabilities = surface.get_capabilities(&adapter);
        if let Some(format) = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
        {
            surface_config.format = format;
            surface_config.view_formats = vec![format];
        }
        surface_config.present_mode = crate::gpu::choose_present_mode(
            config.present_mode,
            &capabilities.present_modes,
            surface_config.present_mode,
        );
        surface.configure(&device, &surface_config);

        let depth = DepthTarget::new(&device, size);
        let scene = SceneRenderer::new(&device, surface_config.format);
        let gui = egui_wgpu::Renderer::new(
            &device,
            surface_config.format,
            egui_wgpu::RendererOptions::default(),
        );
        let info = adapter.get_info();
        let adapter_name = format!("{} · {:?}", info.name, info.backend);
        tracing::info!(
            adapter = %adapter_name,
            driver = %info.driver,
            device_type = ?info.device_type,
            present_mode = ?surface_config.present_mode,
            format = ?surface_config.format,
            "graphics initialized"
        );

        Ok(Self {
            gui,
            scene,
            depth,
            queue,
            device,
            surface: Some(surface),
            adapter,
            instance,
            surface_config,
            size,
            adapter_name,
            needs_reconfigure: false,
        })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Clone lightweight handles for host-owned compute backends. The renderer
    /// still owns presentation; sharing the device avoids a second adapter and
    /// lets `wgpu` schedule compute and rendering on one queue.
    pub(crate) fn compute_handles(&self) -> (wgpu::Device, wgpu::Queue) {
        (self.device.clone(), self.queue.clone())
    }

    fn surface(&self) -> &wgpu::Surface<'static> {
        self.surface
            .as_ref()
            .expect("the surface is only absent while being replaced")
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        self.size = new_size;
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }

        self.surface_config.width = new_size.width;
        self.surface_config.height = new_size.height;
        self.surface().configure(&self.device, &self.surface_config);
        self.depth = DepthTarget::new(&self.device, new_size);
        self.needs_reconfigure = false;
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.size.width, self.size.height)
    }

    pub fn recreate_surface(&mut self, window: Arc<Window>) -> Result<(), RendererInitError> {
        // Drain in-flight work and drop the old surface *before* creating its
        // replacement: two live surfaces for one window is not valid on every
        // backend, and the old one still references the window handle.
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(5)),
        });
        drop(self.surface.take());

        let surface = self.instance.create_surface(window)?;
        let supported = surface
            .get_capabilities(&self.adapter)
            .formats
            .contains(&self.surface_config.format);
        self.surface = Some(surface);

        if supported {
            self.resize(self.size);
            Ok(())
        } else {
            Err(RendererInitError::UnsupportedSurface)
        }
    }

    pub fn render(&mut self, frame: SceneFrame<'_>, gui_paint: GuiPaint<'_>) -> RenderStatus {
        if self.size.width == 0 || self.size.height == 0 {
            return RenderStatus::Skipped;
        }

        if self.needs_reconfigure {
            self.surface().configure(&self.device, &self.surface_config);
            self.needs_reconfigure = false;
        }

        for (texture_id, image_delta) in &gui_paint.textures_delta.set {
            self.gui
                .update_texture(&self.device, &self.queue, *texture_id, image_delta);
        }

        self.scene.update(
            &self.device,
            &self.queue,
            frame.camera,
            frame.viewport.aspect_ratio(),
            frame.instances,
            frame.base,
            frame.overlay,
            frame.time_seconds,
            [frame.viewport.width as f32, frame.viewport.height as f32],
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Field CAD frame encoder"),
            });
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.size.width, self.size.height],
            pixels_per_point: gui_paint.pixels_per_point,
        };
        let user_command_buffers = self.gui.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            gui_paint.primitives,
            &screen_descriptor,
        );

        let acquired = self.surface().get_current_texture();
        let (surface_frame, suboptimal) = match acquired {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            // Not an error, and not something to retry at full rate: the caller
            // backs off instead of spinning on a surface that cannot present.
            wgpu::CurrentSurfaceTexture::Occluded => return RenderStatus::Occluded,
            wgpu::CurrentSurfaceTexture::Timeout => return RenderStatus::Skipped,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.needs_reconfigure = true;
                return RenderStatus::Skipped;
            }
            wgpu::CurrentSurfaceTexture::Lost => return RenderStatus::SurfaceLost,
            wgpu::CurrentSurfaceTexture::Validation => {
                tracing::error!("surface acquisition failed validation");
                return RenderStatus::Skipped;
            }
        };
        self.needs_reconfigure |= suboptimal;

        let view = surface_frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Field CAD scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth.view,
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
            apply_viewport(&mut render_pass, frame.viewport);
            self.scene
                .draw(&mut render_pass, frame.grid_visible, frame.axes_visible);
        }
        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Field CAD UI pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.gui.render(
                &mut render_pass.forget_lifetime(),
                gui_paint.primitives,
                &screen_descriptor,
            );
        }

        self.queue.submit(
            user_command_buffers
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        for texture_id in &gui_paint.textures_delta.free {
            self.gui.free_texture(texture_id);
        }
        surface_frame.present();

        RenderStatus::Presented
    }
}

/// Prefer a real GPU, but fall back to a software adapter rather than refusing
/// to start. A slow viewport is still a usable one; no viewport is not.
async fn request_adapter(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'static>,
    config: crate::gpu::GpuConfig,
) -> Result<wgpu::Adapter, RendererInitError> {
    if config.force_fallback_adapter {
        tracing::info!("FIELDCAD_FORCE_FALLBACK is set; requesting a software adapter");
        return instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: true,
                compatible_surface: Some(surface),
            })
            .await
            .map_err(RendererInitError::NoAdapter);
    }

    let preferred = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(surface),
        })
        .await;

    match preferred {
        Ok(adapter) => Ok(adapter),
        Err(error) => {
            tracing::warn!(
                %error,
                "no hardware adapter available; trying a software fallback"
            );
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: true,
                    compatible_surface: Some(surface),
                })
                .await
                .map_err(RendererInitError::NoAdapter)
        }
    }
}

fn apply_viewport(render_pass: &mut wgpu::RenderPass<'_>, viewport: Viewport) {
    render_pass.set_viewport(
        viewport.x as f32,
        viewport.y as f32,
        viewport.width as f32,
        viewport.height as f32,
        0.0,
        1.0,
    );
    render_pass.set_scissor_rect(viewport.x, viewport.y, viewport.width, viewport.height);
}

struct DepthTarget {
    view: wgpu::TextureView,
}

impl DepthTarget {
    fn new(device: &wgpu::Device, size: PhysicalSize<u32>) -> Self {
        Self {
            view: depth_view(device, size.width, size.height),
        }
    }
}

/// A depth attachment matching the scene pipelines. Shared with the windowless
/// smoke test so it exercises the same depth format and pipeline state.
pub(crate) fn depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Field CAD depth target"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 4],
}

impl From<ColoredVertex> for Vertex {
    fn from(vertex: ColoredVertex) -> Self {
        Self {
            position: vertex.position.to_array(),
            color: vertex.color.to_array(),
        }
    }
}

impl Vertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InstanceRaw {
    model: [[f32; 4]; 4],
    tint: [f32; 4],
}

impl InstanceRaw {
    const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4
    ];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBUTES,
        }
    }

    fn from_instance(instance: &ObjectInstance) -> Self {
        Self {
            model: instance.model.to_cols_array_2d(),
            tint: if instance.selected {
                SELECTED_COLOR
            } else {
                instance
                    .color
                    .map_or(OBJECT_COLOR, |color| [color.r, color.g, color.b, color.a])
            },
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_projection: [[f32; 4]; 4],
}

/// One vertex of a flow-line ribbon quad, expanded to a constant screen-pixel
/// width in the vertex shader from `neighbor`/`side` rather than carrying a
/// pre-widened world-space shape — see `vs_flow_line` in `scene.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FlowLineVertex {
    position: [f32; 3],
    neighbor: [f32; 3],
    side: f32,
    arclength: f32,
    thickness_px: f32,
    speed: f32,
    color: [f32; 4],
}

impl From<FlowRibbonVertex> for FlowLineVertex {
    fn from(vertex: FlowRibbonVertex) -> Self {
        Self {
            position: vertex.position.to_array(),
            neighbor: vertex.neighbor.to_array(),
            side: vertex.side,
            arclength: vertex.arclength,
            thickness_px: vertex.thickness_px,
            speed: vertex.speed,
            color: vertex.color.to_array(),
        }
    }
}

impl FlowLineVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
        0 => Float32x3, 1 => Float32x3, 2 => Float32, 3 => Float32, 4 => Float32, 5 => Float32,
        6 => Float32x4
    ];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// Per-frame state the flow-line shader needs beyond the shared camera:
/// `time_seconds` drives the animated scroll, `viewport_size` (physical
/// pixels) lets the vertex shader convert `thickness_px` into a clip-space
/// offset that reads as a constant width regardless of viewport aspect.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FlowLineUniform {
    time_seconds: f32,
    _padding: f32,
    viewport_size: [f32; 2],
}

pub(crate) struct SceneRenderer {
    line_pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    field_surface_pipeline: wgpu::RenderPipeline,
    flow_line_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    flow_line_uniform_buffer: wgpu::Buffer,
    flow_line_bind_group: wgpu::BindGroup,
    grid_buffer: wgpu::Buffer,
    grid_vertex_count: u32,
    axes_buffer: wgpu::Buffer,
    axes_vertex_count: u32,
    cube_vertex_buffer: wgpu::Buffer,
    cube_index_buffer: wgpu::Buffer,
    cube_index_count: u32,
    sphere_vertex_buffer: wgpu::Buffer,
    sphere_index_buffer: wgpu::Buffer,
    sphere_index_count: u32,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    /// Reused across frames instead of collected fresh — see the analogous
    /// `scratch` field on [`DynamicVertexBuffer`].
    instance_scratch: Vec<InstanceRaw>,
    box_instance_count: u32,
    sphere_instance_count: u32,
    field_surface: DynamicVertexBuffer,
    field_lines: DynamicVertexBuffer,
    flow_lines: DynamicFlowLineBuffer,
}

impl SceneRenderer {
    pub(crate) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Scene camera uniform"),
            contents: bytemuck::bytes_of(&CameraUniform {
                view_projection: Mat4::IDENTITY.to_cols_array_2d(),
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Scene camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Scene camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Scene pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Scene shader"),
            source: wgpu::ShaderSource::Wgsl(SCENE_SHADER.into()),
        });

        let flow_line_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Flow line uniform layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let flow_line_uniform_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Flow line uniform"),
                contents: bytemuck::bytes_of(&FlowLineUniform {
                    time_seconds: 0.0,
                    _padding: 0.0,
                    viewport_size: [1.0, 1.0],
                }),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let flow_line_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Flow line bind group"),
            layout: &flow_line_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: flow_line_uniform_buffer.as_entire_binding(),
            }],
        });
        // Its own layout, not the shared `pipeline_layout`: this pipeline
        // needs the extra flow-line uniform at group 1, which the other
        // pipelines have no use for.
        let flow_line_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Flow line pipeline layout"),
                bind_group_layouts: &[Some(&camera_layout), Some(&flow_line_layout)],
                immediate_size: 0,
            });

        let line_pipeline = create_pipeline(
            device,
            &pipeline_layout,
            &shader,
            color_format,
            "vs_world",
            "fs_main",
            &[Vertex::layout()],
            wgpu::PrimitiveTopology::LineList,
            None,
            false,
        );
        let mesh_pipeline = create_pipeline(
            device,
            &pipeline_layout,
            &shader,
            color_format,
            "vs_instanced",
            "fs_main",
            &[Vertex::layout(), InstanceRaw::layout()],
            wgpu::PrimitiveTopology::TriangleList,
            Some(wgpu::Face::Back),
            false,
        );
        let field_surface_pipeline = create_pipeline(
            device,
            &pipeline_layout,
            &shader,
            color_format,
            "vs_world",
            "fs_main",
            &[Vertex::layout()],
            wgpu::PrimitiveTopology::TriangleList,
            None,
            true,
        );
        let flow_line_pipeline = create_pipeline(
            device,
            &flow_line_pipeline_layout,
            &shader,
            color_format,
            "vs_flow_line",
            "fs_flow_line",
            &[FlowLineVertex::layout()],
            wgpu::PrimitiveTopology::TriangleList,
            // Visible from both sides: a ribbon's winding depends on which
            // way its streamline happened to be traced, not on a notion of
            // "outside" the way a solid mesh has one.
            None,
            true,
        );

        let grid_vertices = grid_vertices();
        let grid_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Construction grid vertices"),
            contents: bytemuck::cast_slice(&grid_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let axes_vertices = axes_vertices();
        let axes_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("World axes vertices"),
            contents: bytemuck::cast_slice(&axes_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let cube_vertices = unit_cube_vertices();
        let cube_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Object proxy vertices"),
            contents: bytemuck::cast_slice(&cube_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let cube_indices = unit_cube_indices();
        let cube_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Object proxy indices"),
            contents: bytemuck::cast_slice(&cube_indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let (sphere_vertices, sphere_indices) = unit_sphere_mesh(12, 18);
        let sphere_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Charged sphere vertices"),
            contents: bytemuck::cast_slice(&sphere_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let sphere_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Charged sphere indices"),
            contents: bytemuck::cast_slice(&sphere_indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let instance_capacity = 16;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Object instances"),
            size: (instance_capacity * std::mem::size_of::<InstanceRaw>()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            line_pipeline,
            mesh_pipeline,
            field_surface_pipeline,
            flow_line_pipeline,
            camera_buffer,
            camera_bind_group,
            flow_line_uniform_buffer,
            flow_line_bind_group,
            grid_buffer,
            grid_vertex_count: grid_vertices.len() as u32,
            axes_buffer,
            axes_vertex_count: axes_vertices.len() as u32,
            cube_vertex_buffer,
            cube_index_buffer,
            cube_index_count: cube_indices.len() as u32,
            sphere_vertex_buffer,
            sphere_index_buffer,
            sphere_index_count: sphere_indices.len() as u32,
            instance_buffer,
            instance_capacity,
            instance_scratch: Vec::new(),
            box_instance_count: 0,
            sphere_instance_count: 0,
            field_surface: DynamicVertexBuffer::new(device, "Field magnitude surface"),
            field_lines: DynamicVertexBuffer::new(device, "Field vector glyphs"),
            flow_lines: DynamicFlowLineBuffer::new(device, "Flow line ribbons"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &OrbitCamera,
        aspect_ratio: f32,
        instances: &[ObjectInstance],
        base: &FieldGeometry,
        overlay: &FieldGeometry,
        time_seconds: f32,
        viewport_size: [f32; 2],
    ) {
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&CameraUniform {
                view_projection: camera.view_projection(aspect_ratio).to_cols_array_2d(),
            }),
        );
        queue.write_buffer(
            &self.flow_line_uniform_buffer,
            0,
            bytemuck::bytes_of(&FlowLineUniform {
                time_seconds,
                _padding: 0.0,
                viewport_size: [viewport_size[0].max(1.0), viewport_size[1].max(1.0)],
            }),
        );

        self.instance_scratch.clear();
        self.instance_scratch.extend(
            instances
                .iter()
                .filter(|instance| instance.mesh == ObjectMesh::Box)
                .map(InstanceRaw::from_instance),
        );
        self.box_instance_count = self.instance_scratch.len() as u32;
        self.instance_scratch.extend(
            instances
                .iter()
                .filter(|instance| instance.mesh == ObjectMesh::Sphere)
                .map(InstanceRaw::from_instance),
        );
        self.sphere_instance_count = self.instance_scratch.len() as u32 - self.box_instance_count;
        if !self.instance_scratch.is_empty() {
            if self.instance_scratch.len() > self.instance_capacity {
                self.instance_capacity = self.instance_scratch.len().next_power_of_two();
                self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Object instances"),
                    size: (self.instance_capacity * std::mem::size_of::<InstanceRaw>())
                        as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instance_scratch),
            );
        }

        self.field_surface.update(
            device,
            queue,
            &base.surface_triangles,
            &overlay.surface_triangles,
        );
        self.field_lines
            .update(device, queue, &base.vector_lines, &overlay.vector_lines);
        self.flow_lines
            .update(device, queue, &base.flow_ribbons, &overlay.flow_ribbons);
    }

    pub(crate) fn draw<'pass>(
        &'pass self,
        render_pass: &mut wgpu::RenderPass<'pass>,
        grid_visible: bool,
        axes_visible: bool,
    ) {
        render_pass.set_bind_group(0, &self.camera_bind_group, &[]);

        if self.field_surface.count > 0 {
            render_pass.set_pipeline(&self.field_surface_pipeline);
            render_pass.set_vertex_buffer(0, self.field_surface.buffer.slice(..));
            render_pass.draw(0..self.field_surface.count, 0..1);
        }

        if self.flow_lines.count > 0 {
            render_pass.set_pipeline(&self.flow_line_pipeline);
            render_pass.set_bind_group(1, &self.flow_line_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.flow_lines.buffer.slice(..));
            render_pass.draw(0..self.flow_lines.count, 0..1);
        }

        render_pass.set_pipeline(&self.line_pipeline);
        if grid_visible {
            render_pass.set_vertex_buffer(0, self.grid_buffer.slice(..));
            render_pass.draw(0..self.grid_vertex_count, 0..1);
        }
        if axes_visible {
            render_pass.set_vertex_buffer(0, self.axes_buffer.slice(..));
            render_pass.draw(0..self.axes_vertex_count, 0..1);
        }
        if self.field_lines.count > 0 {
            render_pass.set_vertex_buffer(0, self.field_lines.buffer.slice(..));
            render_pass.draw(0..self.field_lines.count, 0..1);
        }

        if self.box_instance_count + self.sphere_instance_count == 0 {
            return;
        }
        render_pass.set_pipeline(&self.mesh_pipeline);
        render_pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
        if self.box_instance_count > 0 {
            render_pass.set_vertex_buffer(0, self.cube_vertex_buffer.slice(..));
            render_pass
                .set_index_buffer(self.cube_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..self.cube_index_count, 0, 0..self.box_instance_count);
        }
        if self.sphere_instance_count > 0 {
            render_pass.set_vertex_buffer(0, self.sphere_vertex_buffer.slice(..));
            render_pass.set_index_buffer(
                self.sphere_index_buffer.slice(..),
                wgpu::IndexFormat::Uint16,
            );
            render_pass.draw_indexed(
                0..self.sphere_index_count,
                0,
                self.box_instance_count..self.box_instance_count + self.sphere_instance_count,
            );
        }
    }
}

/// How many `element_size`-sized elements one buffer on this device can hold
/// at most. `capacity` must never be grown past this — including by rounding
/// up to a power of two — or the very next allocation panics regardless of
/// how carefully the vertex *count* was clamped.
fn max_buffer_elements(device: &wgpu::Device, element_size: usize) -> usize {
    max_buffer_elements_for(device.limits().max_buffer_size, element_size)
}

/// The pure arithmetic behind [`max_buffer_elements`], taking the limit as a
/// value so it can be checked against known numbers without a GPU device.
fn max_buffer_elements_for(max_buffer_size: u64, element_size: usize) -> usize {
    (max_buffer_size / element_size as u64) as usize
}

/// Clamp `requested` vertices to what one `wgpu` buffer can actually hold on
/// this device, so an unusually large glyph or streamline count degrades to a
/// partial draw instead of a `Device::create_buffer` validation panic that
/// takes the whole app down with it (`max_buffer_size` is commonly 256 MiB,
/// and a dense flow-line trace over a large volume can exceed that without
/// any single input being individually unreasonable).
///
/// Rounded down to a whole number of `group`-sized primitives — a triangle, a
/// line, a ribbon quad — so truncation never leaves a partial, visibly
/// malformed shape at the cut point.
fn clamp_vertex_count(
    device: &wgpu::Device,
    requested: usize,
    element_size: usize,
    group: usize,
    label: &str,
) -> usize {
    clamp_vertex_count_for(
        device.limits().max_buffer_size,
        requested,
        element_size,
        group,
        label,
    )
}

/// The pure arithmetic behind [`clamp_vertex_count`], taking the limit as a
/// value so it can be checked against known numbers without a GPU device —
/// in particular, against the exact 256 MiB / 56-byte-vertex numbers that
/// crashed the app once already.
fn clamp_vertex_count_for(
    max_buffer_size: u64,
    requested: usize,
    element_size: usize,
    group: usize,
    label: &str,
) -> usize {
    let max_elements = max_buffer_elements_for(max_buffer_size, element_size);
    let max_whole_groups = (max_elements / group) * group;
    if requested > max_whole_groups {
        tracing::warn!(
            label,
            requested,
            limit = max_whole_groups,
            "too much geometry for one GPU buffer this frame; drawing a truncated subset \
             instead of crashing — lower the density/thickness settings driving this",
        );
        max_whole_groups
    } else {
        requested
    }
}

struct DynamicVertexBuffer {
    label: &'static str,
    buffer: wgpu::Buffer,
    capacity: usize,
    count: u32,
    /// Reused across `update` calls instead of collected fresh — `.clear()`
    /// and `.extend()` in place keep this buffer's capacity warm rather than
    /// freeing and reallocating it every frame.
    scratch: Vec<Vertex>,
}

impl DynamicVertexBuffer {
    fn new(device: &wgpu::Device, label: &'static str) -> Self {
        let capacity = 1;
        Self {
            label,
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            capacity,
            count: 0,
            scratch: Vec::new(),
        }
    }

    /// `base` and `overlay` are drawn as one combined buffer — see
    /// [`crate::renderer::SceneFrame`] — with `base` given priority over
    /// `overlay` if together they exceed what one GPU buffer can hold, since
    /// `overlay` is UI extras (authoring proxies, ghosts, the gizmo) rather
    /// than the geometry a channel layer actually publishes.
    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        base: &[ColoredVertex],
        overlay: &[ColoredVertex],
    ) {
        // 6 is safe for either primitive type this buffer is drawn as
        // (`LineList` needs a multiple of 2, `TriangleList` a multiple of 3).
        let allowed = clamp_vertex_count(
            device,
            base.len() + overlay.len(),
            std::mem::size_of::<Vertex>(),
            6,
            self.label,
        );
        self.count = allowed as u32;
        self.scratch.clear();
        if allowed == 0 {
            return;
        }
        if allowed > self.capacity {
            // Never past the device limit — `next_power_of_two` alone can
            // round straight back over it even though `allowed` was already
            // clamped under it.
            self.capacity = allowed
                .next_power_of_two()
                .min(max_buffer_elements(device, std::mem::size_of::<Vertex>()));
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: (self.capacity * std::mem::size_of::<Vertex>()) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let base_allowed = allowed.min(base.len());
        let overlay_allowed = allowed - base_allowed;
        self.scratch
            .extend(base[..base_allowed].iter().copied().map(Vertex::from));
        self.scratch
            .extend(overlay[..overlay_allowed].iter().copied().map(Vertex::from));
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&self.scratch));
    }
}

/// [`DynamicVertexBuffer`]'s counterpart for [`FlowLineVertex`]. A distinct,
/// small type rather than a generic one: the two vertex formats are unlikely
/// to grow more siblings, and a generic buffer would need a trait bound for
/// the `From` conversion this already gets for free from being concrete.
struct DynamicFlowLineBuffer {
    label: &'static str,
    buffer: wgpu::Buffer,
    capacity: usize,
    count: u32,
    /// See the equivalent field on [`DynamicVertexBuffer`].
    scratch: Vec<FlowLineVertex>,
}

impl DynamicFlowLineBuffer {
    fn new(device: &wgpu::Device, label: &'static str) -> Self {
        let capacity = 1;
        Self {
            label,
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<FlowLineVertex>() as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            capacity,
            count: 0,
            scratch: Vec::new(),
        }
    }

    /// See the equivalent doc comment and base/overlay priority rule on
    /// [`DynamicVertexBuffer::update`].
    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        base: &[FlowRibbonVertex],
        overlay: &[FlowRibbonVertex],
    ) {
        // 6 vertices per ribbon quad — see `build_flow_ribbon`.
        let allowed = clamp_vertex_count(
            device,
            base.len() + overlay.len(),
            std::mem::size_of::<FlowLineVertex>(),
            6,
            self.label,
        );
        self.count = allowed as u32;
        self.scratch.clear();
        if allowed == 0 {
            return;
        }
        if allowed > self.capacity {
            // See the equivalent comment in `DynamicVertexBuffer::update`.
            self.capacity = allowed.next_power_of_two().min(max_buffer_elements(
                device,
                std::mem::size_of::<FlowLineVertex>(),
            ));
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: (self.capacity * std::mem::size_of::<FlowLineVertex>())
                    as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let base_allowed = allowed.min(base.len());
        let overlay_allowed = allowed - base_allowed;
        self.scratch.extend(
            base[..base_allowed]
                .iter()
                .copied()
                .map(FlowLineVertex::from),
        );
        self.scratch.extend(
            overlay[..overlay_allowed]
                .iter()
                .copied()
                .map(FlowLineVertex::from),
        );
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&self.scratch));
    }
}

#[allow(clippy::too_many_arguments)]
fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    color_format: wgpu::TextureFormat,
    vertex_entry: &str,
    fragment_entry: &str,
    buffers: &[wgpu::VertexBufferLayout<'_>],
    topology: wgpu::PrimitiveTopology,
    cull_mode: Option<wgpu::Face>,
    transparent: bool,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Scene render pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex_entry),
            buffers,
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology,
            cull_mode,
            front_face: wgpu::FrontFace::Ccw,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(!transparent),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(if transparent {
                    wgpu::BlendState::ALPHA_BLENDING
                } else {
                    wgpu::BlendState::REPLACE
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn grid_vertices() -> Vec<Vertex> {
    let mut vertices = Vec::new();
    let extent = 20.0;
    for coordinate in -20_i32..=20 {
        if coordinate == 0 {
            continue;
        }
        let value = coordinate as f32;
        let color = if coordinate % 5 == 0 {
            [0.22, 0.25, 0.31, 1.0]
        } else {
            [0.105, 0.12, 0.155, 1.0]
        };
        vertices.extend([
            Vertex {
                position: [-extent, value, 0.0],
                color,
            },
            Vertex {
                position: [extent, value, 0.0],
                color,
            },
            Vertex {
                position: [value, -extent, 0.0],
                color,
            },
            Vertex {
                position: [value, extent, 0.0],
                color,
            },
        ]);
    }
    vertices
}

fn axes_vertices() -> [Vertex; 6] {
    const X: [f32; 4] = [0.8, 0.13, 0.16, 1.0];
    const Y: [f32; 4] = [0.17, 0.72, 0.28, 1.0];
    const Z: [f32; 4] = [0.16, 0.38, 0.95, 1.0];
    [
        Vertex {
            position: [-20.0, 0.0, 0.0],
            color: X,
        },
        Vertex {
            position: [20.0, 0.0, 0.0],
            color: X,
        },
        Vertex {
            position: [0.0, -20.0, 0.0],
            color: Y,
        },
        Vertex {
            position: [0.0, 20.0, 0.0],
            color: Y,
        },
        Vertex {
            position: [0.0, 0.0, 0.0],
            color: Z,
        },
        Vertex {
            position: [0.0, 0.0, 5.0],
            color: Z,
        },
    ]
}

/// A unit cube centred on the origin. Per-object size arrives in the instance
/// transform, so there is one mesh regardless of how many objects exist.
fn unit_cube_vertices() -> [Vertex; 8] {
    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    let corner = |x: f32, y: f32, z: f32| Vertex {
        position: [x, y, z],
        color: WHITE,
    };
    [
        corner(-1.0, -1.0, -1.0),
        corner(1.0, -1.0, -1.0),
        corner(1.0, 1.0, -1.0),
        corner(-1.0, 1.0, -1.0),
        corner(-1.0, -1.0, 1.0),
        corner(1.0, -1.0, 1.0),
        corner(1.0, 1.0, 1.0),
        corner(-1.0, 1.0, 1.0),
    ]
}

fn unit_cube_indices() -> [u16; 36] {
    [
        0, 2, 1, 0, 3, 2, // bottom
        4, 5, 6, 4, 6, 7, // top
        0, 1, 5, 0, 5, 4, // front
        1, 2, 6, 1, 6, 5, // right
        2, 3, 7, 2, 7, 6, // back
        3, 0, 4, 3, 4, 7, // left
    ]
}

fn unit_sphere_mesh(stacks: u16, sectors: u16) -> (Vec<Vertex>, Vec<u16>) {
    let stacks = stacks.max(3);
    let sectors = sectors.max(6);
    let mut vertices = Vec::with_capacity((stacks as usize + 1) * (sectors as usize + 1));
    for stack in 0..=stacks {
        let theta = std::f32::consts::PI * f32::from(stack) / f32::from(stacks);
        let radius = theta.sin();
        let z = theta.cos();
        for sector in 0..=sectors {
            let phi = std::f32::consts::TAU * f32::from(sector) / f32::from(sectors);
            vertices.push(Vertex {
                position: [radius * phi.cos(), radius * phi.sin(), z],
                color: [1.0, 1.0, 1.0, 1.0],
            });
        }
    }

    let row = sectors + 1;
    let mut indices = Vec::with_capacity(stacks as usize * sectors as usize * 6);
    for stack in 0..stacks {
        for sector in 0..sectors {
            let a = stack * row + sector;
            let b = (stack + 1) * row + sector;
            let c = a + 1;
            let d = b + 1;
            indices.extend([a, b, c, c, b, d]);
        }
    }
    (vertices, indices)
}

#[cfg(test)]
mod tests {
    use fieldcad_core::ObjectId;
    use glam::Vec3;

    use super::*;

    /// Regression: resizing a field box while its flow lines were enabled
    /// once asked for a 469,762,048-byte "Flow line ribbons" buffer against a
    /// 268,435,456-byte (256 MiB) device limit — a real `Device::create_buffer`
    /// validation panic that crashed the whole app, not merely a slow frame.
    /// 469,762,048 was `8_388_608.next_power_of_two()` vertices at 56 bytes
    /// each: the *count* was never even clamped, so the exact numbers from
    /// that crash are reproduced here.
    #[test]
    fn clamp_vertex_count_never_exceeds_a_256_mib_device_limit() {
        const MAX_BUFFER_SIZE: u64 = 268_435_456;
        const FLOW_LINE_VERTEX_SIZE: usize = 56;

        let allowed = clamp_vertex_count_for(
            MAX_BUFFER_SIZE,
            8_388_608,
            FLOW_LINE_VERTEX_SIZE,
            6,
            "Flow line ribbons",
        );

        assert!(
            (allowed as u64) * (FLOW_LINE_VERTEX_SIZE as u64) <= MAX_BUFFER_SIZE,
            "clamped count must fit the device's actual buffer-size limit: {allowed} vertices"
        );
        assert!(
            allowed.is_multiple_of(6),
            "must stay a whole number of ribbon quads"
        );
        assert!(
            allowed < 8_388_608,
            "the crashing request must actually be reduced"
        );
    }

    /// The same overshoot can happen one step later, purely from
    /// `next_power_of_two()` rounding a capacity back over the limit even
    /// after the vertex count itself was clamped under it — this is the
    /// off-by-one the first fix attempt missed.
    #[test]
    fn max_buffer_elements_rules_out_a_capacity_that_next_power_of_two_would_overshoot() {
        const MAX_BUFFER_SIZE: u64 = 268_435_456;
        const FLOW_LINE_VERTEX_SIZE: usize = 56;

        let allowed = clamp_vertex_count_for(
            MAX_BUFFER_SIZE,
            8_388_608,
            FLOW_LINE_VERTEX_SIZE,
            6,
            "Flow line ribbons",
        );
        let naive_capacity = allowed.next_power_of_two();
        let safe_capacity = naive_capacity.min(max_buffer_elements_for(
            MAX_BUFFER_SIZE,
            FLOW_LINE_VERTEX_SIZE,
        ));

        assert!(
            naive_capacity as u64 * FLOW_LINE_VERTEX_SIZE as u64 > MAX_BUFFER_SIZE,
            "test setup: the naive rounding must actually overshoot for this case"
        );
        assert!(
            safe_capacity as u64 * FLOW_LINE_VERTEX_SIZE as u64 <= MAX_BUFFER_SIZE,
            "the capacity actually allocated must never overshoot the device limit"
        );
    }

    /// Compiles the shader on the CPU, so a broken WGSL edit fails the test run
    /// rather than the first frame on someone's machine. Requires no GPU.
    #[test]
    fn scene_shader_compiles_and_declares_its_entry_points() {
        let module =
            naga::front::wgsl::parse_str(SCENE_SHADER).expect("scene.wgsl must be valid WGSL");

        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        );
        validator
            .validate(&module)
            .expect("scene.wgsl must pass validation");

        let entry_points: Vec<_> = module
            .entry_points
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert!(entry_points.contains(&"vs_world"));
        assert!(entry_points.contains(&"vs_instanced"));
        assert!(entry_points.contains(&"fs_main"));
        assert!(entry_points.contains(&"vs_flow_line"));
        assert!(entry_points.contains(&"fs_flow_line"));
    }

    #[test]
    fn flow_line_vertex_attributes_are_all_distinct_locations() {
        let locations: Vec<_> = FlowLineVertex::ATTRIBUTES
            .iter()
            .map(|attribute| attribute.shader_location)
            .collect();
        assert_eq!(locations, vec![0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn instance_attributes_do_not_collide_with_vertex_attributes() {
        let vertex_locations: Vec<_> = Vertex::ATTRIBUTES
            .iter()
            .map(|attribute| attribute.shader_location)
            .collect();
        let instance_locations: Vec<_> = InstanceRaw::ATTRIBUTES
            .iter()
            .map(|attribute| attribute.shader_location)
            .collect();

        assert_eq!(vertex_locations, vec![0, 1]);
        assert_eq!(instance_locations, vec![2, 3, 4, 5, 6]);
    }

    #[test]
    fn selection_changes_only_the_instance_tint() {
        let instance = ObjectInstance {
            id: ObjectId::new(0),
            model: Mat4::IDENTITY,
            half_extent: Vec3::ONE,
            mesh: ObjectMesh::Box,
            selected: false,
            color: None,
        };
        let selected = ObjectInstance {
            selected: true,
            ..instance
        };

        let plain = InstanceRaw::from_instance(&instance);
        let highlighted = InstanceRaw::from_instance(&selected);

        assert_eq!(plain.model, highlighted.model);
        assert_ne!(plain.tint, highlighted.tint);
        assert_eq!(highlighted.tint, SELECTED_COLOR);
    }

    #[test]
    fn an_unselected_instance_uses_its_own_color_when_set() {
        let color = fieldcad_core::ObjectColor {
            r: 0.9,
            g: 0.1,
            b: 0.1,
            a: 1.0,
        };
        let instance = ObjectInstance {
            id: ObjectId::new(0),
            model: Mat4::IDENTITY,
            half_extent: Vec3::ONE,
            mesh: ObjectMesh::Box,
            selected: false,
            color: Some(color),
        };

        let raw = InstanceRaw::from_instance(&instance);

        assert_eq!(raw.tint, [color.r, color.g, color.b, color.a]);
    }

    #[test]
    fn an_uncolored_instance_falls_back_to_the_default_object_color() {
        let instance = ObjectInstance {
            id: ObjectId::new(0),
            model: Mat4::IDENTITY,
            half_extent: Vec3::ONE,
            mesh: ObjectMesh::Box,
            selected: false,
            color: None,
        };

        assert_eq!(InstanceRaw::from_instance(&instance).tint, OBJECT_COLOR);
    }

    #[test]
    fn selection_overrides_a_set_object_color() {
        let color = fieldcad_core::ObjectColor {
            r: 0.9,
            g: 0.1,
            b: 0.1,
            a: 1.0,
        };
        let instance = ObjectInstance {
            id: ObjectId::new(0),
            model: Mat4::IDENTITY,
            half_extent: Vec3::ONE,
            mesh: ObjectMesh::Box,
            selected: true,
            color: Some(color),
        };

        assert_eq!(InstanceRaw::from_instance(&instance).tint, SELECTED_COLOR);
    }

    #[test]
    fn the_unit_cube_mesh_is_scaled_by_the_instance_not_rebuilt() {
        // One mesh, whatever the object sizes: the vertex buffer is constant.
        let vertices = unit_cube_vertices();

        assert_eq!(vertices.len(), 8);
        for vertex in vertices {
            assert!(vertex.position.iter().all(|axis| axis.abs() == 1.0));
        }
    }

    #[test]
    fn charged_source_sphere_mesh_is_unit_sized_and_indexed() {
        let (vertices, indices) = unit_sphere_mesh(8, 12);

        assert!(vertices.len() > 8);
        assert!(!indices.is_empty());
        assert!(
            indices
                .iter()
                .all(|index| usize::from(*index) < vertices.len())
        );
        for vertex in vertices {
            let position = Vec3::from_array(vertex.position);
            assert!((position.length() - 1.0).abs() < 1.0e-5);
        }
    }
}
