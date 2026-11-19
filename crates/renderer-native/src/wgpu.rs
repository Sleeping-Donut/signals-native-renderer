use scene::{Primitive, Rect, Scene};

use crate::{BackendError, BackendMetrics, FrameStats, PaintBackend};

const SHADER: &str = r#"
struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var output: VsOut;
    output.position = vec4<f32>(input.position, 0.0, 1.0);
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    return input.color;
}
"#;

const VERTEX_BYTES: u64 = 24;
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
        let pipeline = create_pipeline(&device, format);
        let (texture, view) = create_offscreen(&device, width, height, format);
        let vertex_buffer = create_vertex_buffer(&device, INITIAL_VERTEX_BYTES);
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
        let pipeline = create_pipeline(&device, format);
        let vertex_buffer = create_vertex_buffer(&device, INITIAL_VERTEX_BYTES);
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

    /// Read back a headless pixel. Returns `None` for a surface target.
    #[must_use]
    pub fn read_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let texture = match &self.target {
            Target::Offscreen { texture, .. } => texture,
            #[cfg(feature = "window")]
            Target::Surface { .. } => return None,
        };
        let width = self.metrics.physical_width.max(1);
        let height = self.metrics.physical_height.max(1);
        if x >= width || y >= height {
            return None;
        }
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
        let offset = (y * padded + x * 4) as usize;
        let pixel = [
            *data.get(offset)?,
            *data.get(offset + 1)?,
            *data.get(offset + 2)?,
            *data.get(offset + 3)?,
        ];
        drop(data);
        buffer.unmap();
        Some(pixel)
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
        collect_vertices(scene, self.metrics, &mut vertices, &mut stats);

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

fn create_pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scene shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("scene layout"),
        bind_group_layouts: &[],
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
    vertices: &mut Vec<u8>,
    stats: &mut FrameStats,
) {
    for primitive in scene.ops() {
        match primitive {
            Primitive::Fill { rect, color, .. } => {
                push_quad(vertices, metrics, *rect, *color);
                stats.fill_quads += 1;
            }
            Primitive::Border {
                rect, width, color, ..
            } => {
                let mut edges = 0;
                if width.top > 0.0 {
                    push_quad(
                        vertices,
                        metrics,
                        Rect::new(rect.x, rect.y, rect.width, width.top),
                        *color,
                    );
                    edges += 1;
                }
                if width.bottom > 0.0 {
                    push_quad(
                        vertices,
                        metrics,
                        Rect::new(
                            rect.x,
                            rect.y + rect.height - width.bottom,
                            rect.width,
                            width.bottom,
                        ),
                        *color,
                    );
                    edges += 1;
                }
                if width.left > 0.0 {
                    push_quad(
                        vertices,
                        metrics,
                        Rect::new(
                            rect.x,
                            rect.y + width.top,
                            width.left,
                            (rect.height - width.top - width.bottom).max(0.0),
                        ),
                        *color,
                    );
                    edges += 1;
                }
                if width.right > 0.0 {
                    push_quad(
                        vertices,
                        metrics,
                        Rect::new(
                            rect.x + rect.width - width.right,
                            rect.y + width.top,
                            width.right,
                            (rect.height - width.top - width.bottom).max(0.0),
                        ),
                        *color,
                    );
                    edges += 1;
                }
                stats.border_quads += edges;
            }
            Primitive::Text {
                rect,
                layout,
                color,
                ..
            } => {
                for line in &layout.lines {
                    let line_y = rect.y + line.baseline - layout.ascent;
                    for glyph in &line.glyphs {
                        push_quad(
                            vertices,
                            metrics,
                            Rect::new(
                                rect.x + glyph.x,
                                line_y,
                                (glyph.advance * 0.65).max(0.5),
                                (layout.ascent * 0.8).max(0.5),
                            ),
                            *color,
                        );
                        stats.glyph_quads += 1;
                    }
                }
            }
        }
    }
}

fn push_quad(
    vertices: &mut Vec<u8>,
    metrics: BackendMetrics,
    rect: Rect,
    color: renderer_api::Color,
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
    for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y0), (x1, y1), (x0, y1)] {
        push_vertex(vertices, x, y, color);
    }
}

fn push_vertex(vertices: &mut Vec<u8>, x: f32, y: f32, color: renderer_api::Color) {
    for value in [x, y, color.r, color.g, color.b, color.a] {
        vertices.extend_from_slice(&value.to_ne_bytes());
    }
}
