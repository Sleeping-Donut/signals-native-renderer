use scene::{Primitive, Rect, Scene};
use text::{Glyph, ShapedLayout};

use crate::atlas::{GlyphAtlas, ATLAS_SIZE};
use crate::{BackendError, BackendMetrics, FrameStats, PaintBackend};

const SHADER: &str = r#"
struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
};

@group(0) @binding(0) var atlas_texture: texture_2d<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var output: VsOut;
    output.position = vec4<f32>(input.position, 0.0, 1.0);
    output.color = input.color;
    output.uv = input.uv;
    return output;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let mask = textureSample(atlas_texture, atlas_sampler, input.uv).r;
    return vec4<f32>(input.color.rgb, input.color.a * mask);
}
"#;

const VERTEX_BYTES: u64 = 32;
const INITIAL_VERTEX_BYTES: u64 = 4096;

enum Target {
    Offscreen {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
    },
    #[cfg(feature = "window")]
    Surface {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
        frame: Option<wgpu::SurfaceTexture>,
    },
}

/// A wgpu scene backend.
pub struct WgpuBackend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    vertex_capacity: u64,
    target: Target,
    format: wgpu::TextureFormat,
    metrics: BackendMetrics,
    adapter_info: String,
    atlas: GlyphAtlas,
    atlas_texture: wgpu::Texture,
    atlas_bind_group: wgpu::BindGroup,
}

impl WgpuBackend {
    /// Create an offscreen backend. Returns [`BackendError::NoAdapter`] when no
    /// GPU adapter is available, which lets headless CI skip the GPU test.
    pub fn new_headless(width: u32, height: u32, scale: f32) -> Result<Self, BackendError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let (adapter, device, queue) = create_device(&instance, None)?;
        let info = adapter.get_info();
        let adapter_info = format!("{:?} {}", info.backend, info.name);
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let bind_group_layout = create_bind_group_layout(&device);
        let pipeline = create_pipeline(&device, format, &bind_group_layout);
        let (texture, view) = create_offscreen(&device, width, height, format);
        let vertex_buffer = create_vertex_buffer(&device, INITIAL_VERTEX_BYTES);
        let atlas = GlyphAtlas::new();
        let (atlas_texture, atlas_bind_group) =
            create_atlas(&device, &queue, &bind_group_layout, &atlas);
        Ok(Self {
            device,
            queue,
            pipeline,
            vertex_buffer,
            vertex_capacity: INITIAL_VERTEX_BYTES,
            target: Target::Offscreen { texture, view },
            format,
            metrics: BackendMetrics::new(width, height, scale),
            adapter_info,
            atlas,
            atlas_texture,
            atlas_bind_group,
        })
    }

    /// Create a window-surface backend.
    #[cfg(feature = "window")]
    pub fn new_surface(
        window: std::sync::Arc<winit::window::Window>,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<Self, BackendError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window)
            .map_err(|error| BackendError::Surface(error.to_string()))?;
        let (adapter, device, queue) = create_device(&instance, Some(&surface))?;
        let info = adapter.get_info();
        let adapter_info = format!("{:?} {}", info.backend, info.name);
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| capabilities.formats.first().copied())
            .ok_or_else(|| BackendError::Surface("surface has no formats".to_string()))?;
        let present_mode = if capabilities
            .present_modes
            .contains(&wgpu::PresentMode::Fifo)
        {
            wgpu::PresentMode::Fifo
        } else {
            capabilities.present_modes[0]
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let bind_group_layout = create_bind_group_layout(&device);
        let pipeline = create_pipeline(&device, format, &bind_group_layout);
        let vertex_buffer = create_vertex_buffer(&device, INITIAL_VERTEX_BYTES);
        let atlas = GlyphAtlas::new();
        let (atlas_texture, atlas_bind_group) =
            create_atlas(&device, &queue, &bind_group_layout, &atlas);
        Ok(Self {
            device,
            queue,
            pipeline,
            vertex_buffer,
            vertex_capacity: INITIAL_VERTEX_BYTES,
            target: Target::Surface {
                surface,
                config,
                frame: None,
            },
            format,
            metrics: BackendMetrics::new(width.max(1), height.max(1), scale),
            adapter_info,
            atlas,
            atlas_texture,
            atlas_bind_group,
        })
    }

    /// Reconfigure a surface after an outdated/lost error.
    pub fn recover(&mut self) -> Result<(), BackendError> {
        #[cfg(feature = "window")]
        if let Target::Surface {
            surface, config, ..
        } = &mut self.target
        {
            surface.configure(&self.device, config);
        }
        Ok(())
    }

    /// The texture format used by the backend.
    #[must_use]
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// GPU adapter description.
    #[must_use]
    pub fn adapter_info(&self) -> &str {
        &self.adapter_info
    }

    /// Number of glyphs in the atlas.
    #[must_use]
    pub fn atlas_len(&self) -> usize {
        self.atlas.len()
    }

    /// Atlas cache counters.
    #[must_use]
    pub fn atlas_stats(&self) -> crate::atlas::AtlasStats {
        self.atlas.stats()
    }

    /// Read back a headless pixel. Returns `None` for a surface target.
    #[must_use]
    pub fn read_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let (width, height, data) = self.read_image()?;
        if x >= width || y >= height {
            return None;
        }
        let offset = ((y * width + x) * 4) as usize;
        Some([
            *data.get(offset)?,
            *data.get(offset + 1)?,
            *data.get(offset + 2)?,
            *data.get(offset + 3)?,
        ])
    }

    /// Read the whole offscreen image as RGBA rows.
    #[must_use]
    pub fn read_image(&self) -> Option<(u32, u32, Vec<u8>)> {
        let texture = match &self.target {
            Target::Offscreen { texture, .. } => texture,
            #[cfg(feature = "window")]
            Target::Surface { .. } => return None,
        };
        let width = self.metrics.physical_width.max(1);
        let height = self.metrics.physical_height.max(1);
        let unpadded = width * 4;
        let padded = unpadded.div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let _ = self.device.poll(wgpu::Maintain::Wait);
        receiver.recv().ok()?.ok()?;
        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((unpadded * height) as usize);
        for row in 0..height {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        buffer.unmap();
        Some((width, height, pixels))
    }

    fn upload_atlas(&mut self) {
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            self.atlas.data(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE),
                rows_per_image: Some(ATLAS_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
        );
        self.atlas.clear_dirty();
    }

    fn ensure_buffer(&mut self, needed: u64) {
        if needed <= self.vertex_capacity {
            return;
        }
        let mut capacity = self.vertex_capacity.max(INITIAL_VERTEX_BYTES);
        while capacity < needed {
            capacity *= 2;
        }
        self.vertex_buffer = create_vertex_buffer(&self.device, capacity);
        self.vertex_capacity = capacity;
    }

    fn render_target(&mut self) -> Result<wgpu::TextureView, BackendError> {
        match &mut self.target {
            Target::Offscreen { texture, .. } => {
                Ok(texture.create_view(&wgpu::TextureViewDescriptor::default()))
            }
            #[cfg(feature = "window")]
            Target::Surface {
                surface,
                config,
                frame,
            } => {
                let acquired = match surface.get_current_texture() {
                    Ok(frame) => frame,
                    Err(wgpu::SurfaceError::Timeout) => return Err(BackendError::Timeout),
                    Err(wgpu::SurfaceError::Outdated) => {
                        surface.configure(&self.device, config);
                        surface
                            .get_current_texture()
                            .map_err(|_| BackendError::Outdated)?
                    }
                    Err(wgpu::SurfaceError::Lost) => return Err(BackendError::Lost),
                    Err(wgpu::SurfaceError::OutOfMemory) => return Err(BackendError::OutOfMemory),
                };
                let view = acquired
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                *frame = Some(acquired);
                Ok(view)
            }
        }
    }
}

impl PaintBackend for WgpuBackend {
    fn resize(&mut self, width: u32, height: u32, scale: f32) {
        let width = width.max(1);
        let height = height.max(1);
        self.metrics = BackendMetrics::new(width, height, scale);
        match &mut self.target {
            Target::Offscreen { texture, view } => {
                let (new_texture, new_view) =
                    create_offscreen(&self.device, width, height, self.format);
                *texture = new_texture;
                *view = new_view;
            }
            #[cfg(feature = "window")]
            Target::Surface {
                surface, config, ..
            } => {
                config.width = width;
                config.height = height;
                surface.configure(&self.device, config);
            }
        }
    }

    fn render(&mut self, scene: &Scene, damage: &[Rect]) -> Result<FrameStats, BackendError> {
        let mut vertices = Vec::with_capacity(scene.len() * 6 * 6 * 4);
        let mut stats = FrameStats {
            primitives: scene.len(),
            damage_rects: damage.len(),
            ..FrameStats::default()
        };
        collect_vertices(
            scene,
            self.metrics,
            &mut self.atlas,
            &mut vertices,
            &mut stats,
        );
        if self.atlas.is_dirty() {
            self.upload_atlas();
        }

        self.ensure_buffer(vertices.len() as u64);
        if !vertices.is_empty() {
            self.queue
                .write_buffer(&self.vertex_buffer, 0, vertices.as_slice());
        }

        let view = self.render_target()?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scene encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.06,
                            g: 0.06,
                            b: 0.07,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if !vertices.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.atlas_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.draw(0..(vertices.len() as u32 / VERTEX_BYTES as u32), 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        Ok(stats)
    }

    fn present(&mut self) -> Result<(), BackendError> {
        #[cfg(feature = "window")]
        if let Target::Surface { frame, .. } = &mut self.target {
            if let Some(frame) = frame.take() {
                frame.present();
            }
        }
        Ok(())
    }

    fn metrics(&self) -> BackendMetrics {
        self.metrics
    }

    fn read_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        WgpuBackend::read_pixel(self, x, y)
    }

    fn name(&self) -> &'static str {
        "wgpu"
    }
}

fn create_device(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), BackendError> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: surface,
        force_fallback_adapter: false,
    }))
    .ok_or(BackendError::NoAdapter)?;
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("signals-native"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
        },
        None,
    ))
    .map_err(|error| BackendError::Device(error.to_string()))?;
    Ok((adapter, device, queue))
}

fn create_offscreen(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn create_vertex_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scene vertices"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("atlas layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn create_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    atlas: &GlyphAtlas,
) -> (wgpu::Texture, wgpu::BindGroup) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph atlas"),
        size: wgpu::Extent3d {
            width: ATLAS_SIZE,
            height: ATLAS_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        atlas.data(),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(ATLAS_SIZE),
            rows_per_image: Some(ATLAS_SIZE),
        },
        wgpu::Extent3d {
            width: ATLAS_SIZE,
            height: ATLAS_SIZE,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("atlas sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..wgpu::SamplerDescriptor::default()
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("atlas bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    (texture, bind_group)
}

fn create_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scene shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("scene layout"),
        bind_group_layouts: &[bind_group_layout],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("scene pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: VERTEX_BYTES,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 8,
                        shader_location: 1,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 24,
                        shader_location: 2,
                    },
                ],
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    })
}

fn collect_vertices(
    scene: &Scene,
    metrics: BackendMetrics,
    atlas: &mut GlyphAtlas,
    vertices: &mut Vec<u8>,
    stats: &mut FrameStats,
) {
    let white = atlas.white_uv();
    for primitive in scene.ops() {
        match primitive {
            Primitive::Fill {
                rect, color, clip, ..
            } => {
                if let Some(rect) = apply_clip(*rect, *clip) {
                    push_quad(vertices, metrics, rect, *color, white);
                    stats.fill_quads += 1;
                }
            }
            Primitive::Border {
                rect,
                width,
                color,
                clip,
                ..
            } => {
                let mut edges = 0;
                let mut edge = |edge_rect: Rect| {
                    if let Some(edge_rect) = apply_clip(edge_rect, *clip) {
                        push_quad(vertices, metrics, edge_rect, *color, white);
                        edges += 1;
                    }
                };
                if width.top > 0.0 {
                    edge(Rect::new(rect.x, rect.y, rect.width, width.top));
                }
                if width.bottom > 0.0 {
                    edge(Rect::new(
                        rect.x,
                        rect.y + rect.height - width.bottom,
                        rect.width,
                        width.bottom,
                    ));
                }
                if width.left > 0.0 {
                    edge(Rect::new(
                        rect.x,
                        rect.y + width.top,
                        width.left,
                        (rect.height - width.top - width.bottom).max(0.0),
                    ));
                }
                if width.right > 0.0 {
                    edge(Rect::new(
                        rect.x + rect.width - width.right,
                        rect.y + width.top,
                        width.right,
                        (rect.height - width.top - width.bottom).max(0.0),
                    ));
                }
                stats.border_quads += edges;
            }
            Primitive::Caret {
                rect, color, clip, ..
            } => {
                if let Some(rect) = apply_clip(*rect, *clip) {
                    push_quad(vertices, metrics, rect, *color, white);
                    stats.caret_quads += 1;
                }
            }
            Primitive::Scrollbar {
                track,
                thumb,
                track_color,
                thumb_color,
                ..
            } => {
                if let Some(track) = apply_clip(*track, None) {
                    push_quad(vertices, metrics, track, *track_color, white);
                    if let Some(thumb) = apply_clip(*thumb, None) {
                        push_quad(vertices, metrics, thumb, *thumb_color, white);
                    }
                    stats.scrollbar_quads += 2;
                }
            }
            Primitive::Tooltip {
                rect,
                layout,
                color,
                background,
                border,
                padding,
                clip,
                ..
            } => {
                if let Some(rect) = apply_clip(*rect, *clip) {
                    push_quad(vertices, metrics, rect, *background, white);
                    stats.tooltip_quads += 1;
                }
                if let Some(outer) = apply_clip(*rect, *clip) {
                    let mut edges = 0;
                    let mut edge = |edge_rect: Rect| {
                        if let Some(edge_rect) = apply_clip(edge_rect, *clip) {
                            push_quad(vertices, metrics, edge_rect, *border, white);
                            edges += 1;
                        }
                    };
                    edge(Rect::new(outer.x, outer.y, outer.width, 1.0));
                    edge(Rect::new(
                        outer.x,
                        outer.y + outer.height - 1.0,
                        outer.width,
                        1.0,
                    ));
                    edge(Rect::new(outer.x, outer.y, 1.0, outer.height));
                    edge(Rect::new(
                        outer.x + outer.width - 1.0,
                        outer.y,
                        1.0,
                        outer.height,
                    ));
                    stats.border_quads += edges;
                }
                let text_rect = Rect::new(
                    rect.x + padding.left,
                    rect.y + padding.top,
                    layout.width,
                    layout.height,
                );
                draw_text_run(
                    vertices,
                    metrics,
                    atlas,
                    text_rect,
                    layout,
                    *color,
                    *clip,
                    renderer_api::Transform::IDENTITY,
                    stats,
                );
            }
            Primitive::Text {
                rect,
                layout,
                color,
                clip,
                transform,
                underline,
                ..
            } => {
                draw_text_run(
                    vertices, metrics, atlas, *rect, layout, *color, *clip, *transform, stats,
                );
                if *underline && layout.width > 0.0 {
                    let underline_rect = Rect::new(
                        rect.x,
                        rect.y + layout.height.max(1.0) - 1.0,
                        layout.width,
                        1.0,
                    )
                    .transformed(*transform);
                    if let Some(underline_rect) = apply_clip(underline_rect, *clip) {
                        push_quad(vertices, metrics, underline_rect, *color, white);
                        stats.underline_quads += 1;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text_run(
    vertices: &mut Vec<u8>,
    metrics: BackendMetrics,
    atlas: &mut GlyphAtlas,
    rect: Rect,
    layout: &ShapedLayout,
    color: renderer_api::Color,
    clip: Option<Rect>,
    transform: renderer_api::Transform,
    stats: &mut FrameStats,
) {
    let white = atlas.white_uv();
    for line in &layout.lines {
        let line_y = rect.y + line.baseline - layout.ascent;
        for glyph in &line.glyphs {
            if !crate::is_paintable_glyph(glyph) {
                continue;
            }
            if let Some(image) = &glyph.image {
                if let Some(entry) = atlas.ensure(image) {
                    let glyph_rect = draw_glyph(
                        vertices, metrics, &rect, line_y, glyph, color, entry, clip, transform,
                    );
                    if glyph_rect {
                        stats.atlas_glyphs += 1;
                    }
                    continue;
                }
            }
            let placeholder = placeholder_glyph(&rect, line_y, glyph, layout.ascent);
            if let Some(placeholder) = apply_clip(placeholder.transformed(transform), clip) {
                push_quad(vertices, metrics, placeholder, color, white);
                stats.placeholder_glyphs += 1;
            }
        }
    }
    stats.glyph_quads += layout
        .lines
        .iter()
        .flat_map(|line| &line.glyphs)
        .filter(|glyph| crate::is_paintable_glyph(glyph))
        .count();
}

fn placeholder_glyph(rect: &Rect, line_y: f32, glyph: &Glyph, ascent: f32) -> Rect {
    Rect::new(
        rect.x + glyph.x,
        line_y,
        (glyph.advance * 0.65).max(0.5),
        (ascent * 0.8).max(0.5),
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_glyph(
    vertices: &mut Vec<u8>,
    metrics: BackendMetrics,
    rect: &Rect,
    line_y: f32,
    glyph: &Glyph,
    color: renderer_api::Color,
    entry: crate::atlas::AtlasEntry,
    clip: Option<Rect>,
    transform: renderer_api::Transform,
) -> bool {
    let glyph_rect = Rect::new(
        rect.x + glyph.x + entry.left,
        line_y + glyph.y - entry.top,
        entry.width,
        entry.height,
    )
    .transformed(transform);
    let Some(glyph_rect) = apply_clip(glyph_rect, clip) else {
        return false;
    };
    push_textured_quad(vertices, metrics, glyph_rect, color, entry.uv);
    true
}

fn apply_clip(rect: Rect, clip: Option<Rect>) -> Option<Rect> {
    match clip {
        Some(clip) => rect.intersect(clip),
        None => Some(rect),
    }
}

fn push_quad(
    vertices: &mut Vec<u8>,
    metrics: BackendMetrics,
    rect: Rect,
    color: renderer_api::Color,
    uv: [f32; 2],
) {
    push_textured_quad(vertices, metrics, rect, color, [uv[0], uv[1], uv[0], uv[1]]);
}

fn push_textured_quad(
    vertices: &mut Vec<u8>,
    metrics: BackendMetrics,
    rect: Rect,
    color: renderer_api::Color,
    uv: [f32; 4],
) {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let width = metrics.physical_width.max(1) as f32;
    let height = metrics.physical_height.max(1) as f32;
    let scale = metrics.scale;
    let x0 = rect.x * scale / width * 2.0 - 1.0;
    let x1 = (rect.x + rect.width) * scale / width * 2.0 - 1.0;
    let y0 = 1.0 - rect.y * scale / height * 2.0;
    let y1 = 1.0 - (rect.y + rect.height) * scale / height * 2.0;
    let [u0, v0, u1, v1] = uv;
    for (x, y, u, v) in [
        (x0, y0, u0, v0),
        (x1, y0, u1, v0),
        (x1, y1, u1, v1),
        (x0, y0, u0, v0),
        (x1, y1, u1, v1),
        (x0, y1, u0, v1),
    ] {
        push_vertex(vertices, x, y, color, [u, v]);
    }
}

fn push_vertex(vertices: &mut Vec<u8>, x: f32, y: f32, color: renderer_api::Color, uv: [f32; 2]) {
    for value in [x, y, color.r, color.g, color.b, color.a, uv[0], uv[1]] {
        vertices.extend_from_slice(&value.to_ne_bytes());
    }
}
