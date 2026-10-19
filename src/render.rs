//! wgpu renderer: a compute pass that refreshes normals of dirty chunks, one pipeline for the
//! sculpt mesh, and the egui overlay.

use crate::mesh::SculptMesh;
use rayon::prelude::*;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub view_proj: [[f32; 4]; 4],
    pub inv_view_proj: [[f32; 4]; 4],
    pub cam_pos: [f32; 4],
    pub light_dir: [f32; 4],
    pub brush: [f32; 4],
    pub params: [f32; 4],
    pub viewport: [f32; 4],
    pub display: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NormalParams {
    vertex_count: u32,
    _pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GrabParams {
    delta: [f32; 3],
    count: u32,
}

/// A grab stroke running on the GPU.
struct GpuGrab {
    bind_group: wgpu::BindGroup,
    count: u32,
    /// Chunk flags for the normal pass while the grab is active.
    flags: Vec<u32>,
    pending: Option<[f32; 3]>,
    // Kept alive for the bind group.
    _buffers: [wgpu::Buffer; 3],
}

/// GPU copy of one subdivision level.
struct MeshBuffers {
    positions: wgpu::Buffer,
    normals: wgpu::Buffer,
    indices: wgpu::Buffer,
    chunk_dirty: wgpu::Buffer,
    normal_bind_group: wgpu::BindGroup,
    index_count: u32,
    vertex_count: u32,
    /// Per-chunk flags for the normal recompute pass.
    dirty_flags: Vec<u32>,
    /// GPU memory held by this level.
    bytes: u64,
    // Kept alive for the bind group.
    _normal_params: wgpu::Buffer,
}

/// Levels that are not shown keep their GPU copies while they total at most this much;
/// beyond it the largest ones are dropped and re-uploaded on their next visit. A
/// 61M-vertex level alone is 4.4 GB, so this is what lets it fit next to the lower levels
/// on an 8 GB card.
const GPU_CACHE_BYTES: u64 = 1 << 30;

/// Uploads are flushed to the GPU in pieces of this size so the staging memory in flight
/// stays bounded.
const UPLOAD_PIECE: usize = 256 << 20;

pub struct Gfx {
    surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    depth_view: wgpu::TextureView,
    pipeline: wgpu::RenderPipeline,
    background_pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    normal_pipeline: wgpu::ComputePipeline,
    normal_bgl: wgpu::BindGroupLayout,
    grab_pipeline: wgpu::ComputePipeline,
    grab_bgl: wgpu::BindGroupLayout,
    grab_params: wgpu::Buffer,
    grab: Option<GpuGrab>,
    /// One entry per subdivision level, uploaded the first time the level is shown so
    /// stepping between levels is a pointer swap.
    levels: Vec<Option<MeshBuffers>>,
    active: usize,
    needs_normals: bool,
    pub egui_renderer: egui_wgpu::Renderer,
    pub adapter_name: String,
    /// Bytes uploaded by the last `upload_dirty`.
    pub last_upload_bytes: usize,
    pub last_upload_chunks: usize,
    pub last_normal_chunks: usize,
}

impl Gfx {
    pub fn new(window: Arc<Window>, vsync: bool) -> Self {
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
                ..wgpu::InstanceDescriptor::new_with_display_handle(Box::new(window.clone()))
            }
            .with_env(),
        );
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("no compatible GPU adapter");
        let info = adapter.get_info();
        let adapter_name = format!("{} ({:?})", info.name, info.backend);
        log::info!("adapter: {adapter_name}");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("klay"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .expect("request device");

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        // egui prefers a non-sRGB swapchain; the mesh shader applies gamma itself.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("surface unsupported");
        config.format = format;
        config.present_mode = if vsync {
            wgpu::PresentMode::AutoVsync
        } else {
            wgpu::PresentMode::AutoNoVsync
        };
        surface.configure(&device, &config);

        let depth_view = create_depth(&device, config.width, config.height);

        // --- Mesh pipeline -------------------------------------------------------------
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniform bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniform bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pos_attr = [wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        }];
        let nrm_attr = [wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Snorm8x4,
            offset: 0,
            shader_location: 1,
        }];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 12,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &pos_attr,
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 4,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &nrm_attr,
                    }),
                ],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let background_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport background"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_background"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_background"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            primitive: Default::default(),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- Normal compute pipeline ----------------------------------------------------
        let normal_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("normals"),
            source: wgpu::ShaderSource::Wgsl(include_str!("normals.wgsl").into()),
        });
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let normal_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("normals bgl"),
            entries: &[
                storage(0, true),
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, true),
                storage(5, true),
                storage(6, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let normal_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("normals layout"),
            bind_group_layouts: &[Some(&normal_bgl)],
            immediate_size: 0,
        });
        let normal_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("normals pipeline"),
            layout: Some(&normal_layout),
            module: &normal_shader,
            entry_point: Some("cs_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        // --- Grab compute pipeline ------------------------------------------------------
        let grab_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grab"),
            source: wgpu::ShaderSource::Wgsl(include_str!("grab.wgsl").into()),
        });
        let grab_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grab bgl"),
            entries: &[
                storage(0, false),
                storage(1, true),
                storage(2, true),
                storage(3, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let grab_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grab layout"),
            bind_group_layouts: &[Some(&grab_bgl)],
            immediate_size: 0,
        });
        let grab_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("grab pipeline"),
            layout: Some(&grab_layout),
            module: &grab_shader,
            entry_point: Some("cs_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let grab_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grab params"),
            size: std::mem::size_of::<GrabParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            config.format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                ..Default::default()
            },
        );

        Self {
            surface,
            device,
            queue,
            config,
            depth_view,
            pipeline,
            uniform_buf,
            background_pipeline,
            bind_group,
            normal_pipeline,
            normal_bgl,
            grab_pipeline,
            grab_bgl,
            grab_params,
            grab: None,
            levels: Vec::new(),
            active: 0,
            needs_normals: false,
            egui_renderer,
            adapter_name,
            last_upload_bytes: 0,
            last_upload_chunks: 0,
            last_normal_chunks: 0,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = create_depth(&self.device, width, height);
    }

    /// Forget every uploaded level (the document was replaced).
    pub fn clear_levels(&mut self) {
        self.levels.clear();
        self.active = 0;
        self.grab = None;
    }

    /// Drop the GPU copies of levels at or above `count` (a level was removed).
    pub fn truncate_levels(&mut self, count: usize) {
        self.levels.truncate(count);
        self.active = self.active.min(count.saturating_sub(1));
    }

    /// Display `level`, uploading it on first sight. Positions changed since the level was
    /// last shown are re-uploaded by the next `upload_dirty`. Returns false when the level
    /// does not fit in GPU memory; the previously shown level stays up.
    pub fn show_level(&mut self, mesh: &mut SculptMesh, level: usize) -> bool {
        if self.levels.get(level).is_some_and(Option::is_some) {
            self.active = level;
            self.trim_cache();
            true
        } else {
            self.upload_level(mesh, level)
        }
    }

    fn active_buffers(&self) -> Option<&MeshBuffers> {
        self.levels.get(self.active).and_then(Option::as_ref)
    }

    /// GPU memory held by the levels, shown or cached.
    pub fn level_bytes(&self) -> u64 {
        self.levels.iter().flatten().map(|m| m.bytes).sum()
    }

    /// Drop cached copies of levels that are not shown, largest first, until they fit
    /// `GPU_CACHE_BYTES`.
    fn trim_cache(&mut self) {
        loop {
            let cached = self
                .levels
                .iter()
                .enumerate()
                .filter(|(i, l)| *i != self.active && l.is_some());
            let total: u64 = cached.clone().map(|(_, l)| l.as_ref().unwrap().bytes).sum();
            if total <= GPU_CACHE_BYTES {
                return;
            }
            let Some((victim, _)) = cached.max_by_key(|(_, l)| l.as_ref().unwrap().bytes) else {
                return;
            };
            log::info!("gpu: dropping the cached copy of level {victim}");
            self.levels[victim] = None;
        }
    }

    /// Drop every cached level except the shown one and wait for the memory to be released.
    fn evict_cached(&mut self) {
        for (i, l) in self.levels.iter_mut().enumerate() {
            if i != self.active {
                *l = None;
            }
        }
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }

    /// Create a buffer and fill it in pieces, flushing between them so the staging memory
    /// in flight stays bounded.
    fn upload(&self, label: &str, data: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: data.len().max(4) as u64,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        for (i, piece) in data.chunks(UPLOAD_PIECE).enumerate() {
            self.queue
                .write_buffer(&buffer, (i * UPLOAD_PIECE) as u64, piece);
            if piece.len() == UPLOAD_PIECE {
                self.queue.submit(std::iter::empty());
                let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
            }
        }
        buffer
    }

    /// Upload a level in full and display it. Normals are computed on the GPU on the next
    /// frame. When the level does not fit, every cached level is dropped and the upload is
    /// tried once more; if it still fails the level is not shown and false is returned.
    pub fn upload_level(&mut self, mesh: &mut SculptMesh, level: usize) -> bool {
        let previous = self.active;
        if self.levels.len() <= level {
            self.levels.resize_with(level + 1, || None);
        }
        self.levels[level] = None;
        self.active = level;
        self.trim_cache();
        for attempt in 0..2 {
            match self.create_level(mesh) {
                Some(buffers) => {
                    log::info!(
                        "gpu: level {level} uploaded, {} MB",
                        buffers.bytes / 1_000_000
                    );
                    self.levels[level] = Some(buffers);
                    for ch in &mut mesh.chunks {
                        ch.dirty_pos = false;
                        ch.dirty_nrm = true;
                    }
                    return true;
                }
                None if attempt == 0 => self.evict_cached(),
                None => {}
            }
        }
        self.active = previous;
        false
    }

    /// Buffers for a level, or `None` when the GPU is out of memory.
    fn create_level(&self, mesh: &SculptMesh) -> Option<MeshBuffers> {
        use wgpu::BufferUsages as U;
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let positions = self.upload(
            "positions",
            bytemuck::cast_slice(&mesh.positions),
            U::VERTEX | U::STORAGE,
        );
        let indices = self.upload(
            "indices",
            bytemuck::cast_slice(&mesh.indices),
            U::INDEX | U::STORAGE,
        );
        let vt_offsets = self.upload(
            "vt_offsets",
            bytemuck::cast_slice(&mesh.vt_offsets),
            U::STORAGE,
        );
        let vt_tris = self.upload("vt_tris", bytemuck::cast_slice(&mesh.vt_tris), U::STORAGE);
        let vert_chunk = self.upload(
            "vert_chunk",
            bytemuck::cast_slice(&mesh.vert_chunk),
            U::STORAGE,
        );
        let normals = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("normals"),
            size: (mesh.vertex_count() * 4).max(4) as u64,
            usage: U::VERTEX | U::STORAGE,
            mapped_at_creation: false,
        });
        let chunk_dirty = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chunk_dirty"),
            size: (mesh.chunks.len().max(1) * 4) as u64,
            usage: U::STORAGE | U::COPY_DST,
            mapped_at_creation: false,
        });
        let normal_params = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("normal params"),
                contents: bytemuck::bytes_of(&NormalParams {
                    vertex_count: mesh.vertex_count() as u32,
                    _pad: [0; 3],
                }),
                usage: U::UNIFORM,
            });
        let bytes = [
            &positions,
            &indices,
            &vt_offsets,
            &vt_tris,
            &vert_chunk,
            &normals,
            &chunk_dirty,
        ]
        .iter()
        .map(|b| b.size())
        .sum();
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("gpu: a level of {} MB did not fit: {e}", bytes / 1_000_000);
            drop((
                positions,
                indices,
                vt_offsets,
                vt_tris,
                vert_chunk,
                normals,
                chunk_dirty,
                normal_params,
            ));
            let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
            return None;
        }
        let normal_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("normals bg"),
            layout: &self.normal_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: positions.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: indices.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: vt_offsets.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: vt_tris.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: vert_chunk.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: chunk_dirty.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: normals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: normal_params.as_entire_binding(),
                },
            ],
        });
        Some(MeshBuffers {
            positions,
            normals,
            indices,
            chunk_dirty,
            normal_bind_group,
            index_count: mesh.indices.len() as u32,
            vertex_count: mesh.vertex_count() as u32,
            dirty_flags: vec![0; mesh.chunks.len()],
            bytes,
            _normal_params: normal_params,
        })
    }

    /// Start a grab on the GPU: upload the selection, its original positions and weights once.
    pub fn begin_gpu_grab(&mut self, setup: crate::sculpt::GpuGrabSetup<'_>, flags: Vec<u32>) {
        let Some(m) = self.active_buffers() else {
            return;
        };
        use wgpu::BufferUsages as U;
        let init = |label: &str, data: &[u8]| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: data,
                    usage: U::STORAGE,
                })
        };
        let sel = init("grab sel", bytemuck::cast_slice(setup.verts));
        let orig = init("grab orig", bytemuck::cast_slice(setup.orig));
        let weights = init("grab weights", bytemuck::cast_slice(setup.weights));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grab bg"),
            layout: &self.grab_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: m.positions.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: sel.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: orig.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: weights.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.grab_params.as_entire_binding(),
                },
            ],
        });
        self.grab = Some(GpuGrab {
            bind_group,
            count: setup.verts.len() as u32,
            flags,
            pending: None,
            _buffers: [sel, orig, weights],
        });
    }

    /// Queue a grab delta for the next frame.
    pub fn gpu_grab_delta(&mut self, delta: glam::Vec3) {
        if let Some(g) = self.grab.as_mut() {
            g.pending = Some(delta.to_array());
        }
    }

    pub fn end_gpu_grab(&mut self) {
        self.grab = None;
    }

    pub fn gpu_grab_active(&self) -> bool {
        self.grab.is_some()
    }

    /// Re-upload positions of chunks whose vertices moved (runs of consecutive dirty chunks
    /// are merged into one copy each) and flag chunks whose normals must be recomputed.
    pub fn upload_dirty(&mut self, mesh: &mut SculptMesh) {
        let Some(buffers) = self.levels.get_mut(self.active).and_then(Option::as_mut) else {
            return;
        };
        debug_assert_eq!(buffers.dirty_flags.len(), mesh.chunks.len());
        let mut count = 0;
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut normal_chunks = 0;
        for (i, ch) in mesh.chunks.iter_mut().enumerate() {
            let nrm = std::mem::take(&mut ch.dirty_nrm);
            buffers.dirty_flags[i] = nrm as u32;
            normal_chunks += nrm as usize;
            if !ch.dirty_pos {
                continue;
            }
            ch.dirty_pos = false;
            count += 1;
            let (s, e) = ch.range();
            match runs.last_mut() {
                Some(last) if last.1 == s => last.1 = e,
                _ => runs.push((s, e)),
            }
        }
        // One staging view per run, filled from several threads: the memcpy into staging
        // memory is the dominant per-frame cost for large strokes.
        const PIECE: usize = 1 << 20;
        let mut bytes = 0;
        let mut t_alloc = 0.0f32;
        let mut t_fill = 0.0f32;
        let mut t_drop = 0.0f32;
        for &(s, e) in &runs {
            let src: &[u8] = bytemuck::cast_slice(&mesh.positions[s..e]);
            let Some(size) = std::num::NonZeroU64::new(src.len() as u64) else {
                continue;
            };
            let t0 = std::time::Instant::now();
            let Some(mut view) =
                self.queue
                    .write_buffer_with(&buffers.positions, (s * 12) as u64, size)
            else {
                continue;
            };
            let t1 = std::time::Instant::now();
            // `WriteOnly<[u8]>` is not Send, but fixed-size pieces `WriteOnly<[u8; N]>` are.
            let (pieces, mut rest) = view.slice(..).into_chunks::<PIECE>();
            let pieces: Vec<wgpu::WriteOnly<[u8; PIECE]>> = pieces.into_iter().collect();
            let full = pieces.len() * PIECE;
            pieces.into_par_iter().enumerate().for_each(|(i, piece)| {
                let mut dst: wgpu::WriteOnly<[u8]> = piece.into();
                dst.copy_from_slice(&src[i * PIECE..(i + 1) * PIECE]);
            });
            rest.copy_from_slice(&src[full..]);
            bytes += src.len();
            let t2 = std::time::Instant::now();
            drop(view);
            let t3 = std::time::Instant::now();
            t_alloc += (t1 - t0).as_secs_f32() * 1000.0;
            t_fill += (t2 - t1).as_secs_f32() * 1000.0;
            t_drop += (t3 - t2).as_secs_f32() * 1000.0;
        }
        if bytes > 16 << 20 {
            log::debug!(
                "upload {:.1} MB in {} runs: alloc {t_alloc:.2} ms, fill {t_fill:.2} ms, drop {t_drop:.2} ms",
                bytes as f32 / 1e6,
                runs.len()
            );
        }
        if let Some(g) = &self.grab {
            if g.pending.is_some() {
                for (f, &gf) in buffers.dirty_flags.iter_mut().zip(&g.flags) {
                    *f |= gf;
                }
                normal_chunks = g.flags.iter().filter(|&&f| f != 0).count();
            }
        }
        if normal_chunks > 0 {
            self.queue.write_buffer(
                &buffers.chunk_dirty,
                0,
                bytemuck::cast_slice(&buffers.dirty_flags),
            );
        }
        self.needs_normals = normal_chunks > 0;
        self.last_normal_chunks = normal_chunks;
        self.last_upload_bytes = bytes;
        self.last_upload_chunks = count;
    }

    pub fn render(
        &mut self,
        uniforms: &Uniforms,
        paint_jobs: &[egui::ClippedPrimitive],
        mut textures_delta: egui::TexturesDelta,
        pixels_per_point: f32,
    ) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(uniforms));

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        // GPU grab: move the selected vertices.
        if let Some(g) = self.grab.as_mut() {
            if let Some(delta) = g.pending.take() {
                self.queue.write_buffer(
                    &self.grab_params,
                    0,
                    bytemuck::bytes_of(&GrabParams {
                        delta,
                        count: g.count,
                    }),
                );
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("grab"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.grab_pipeline);
                pass.set_bind_group(0, &g.bind_group, &[]);
                let groups = g.count.div_ceil(256);
                pass.dispatch_workgroups(groups.min(65535), groups.div_ceil(65535), 1);
            }
        }

        // Normal recompute for dirty chunks.
        let needs_normals = std::mem::take(&mut self.needs_normals);
        if let (Some(m), true) = (self.active_buffers(), needs_normals) {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("normals"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.normal_pipeline);
            pass.set_bind_group(0, &m.normal_bind_group, &[]);
            let groups = m.vertex_count.div_ceil(256);
            pass.dispatch_workgroups(groups.min(65535), groups.div_ceil(65535), 1);
        }

        // Mesh pass.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mesh"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: crate::workspace::BACKGROUND.r() as f64 / 255.0,
                            g: crate::workspace::BACKGROUND.g() as f64 / 255.0,
                            b: crate::workspace::BACKGROUND.b() as f64 / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            let [x, y, width, height] = uniforms.viewport;
            let x = x.clamp(0.0, (self.config.width - 1) as f32);
            let y = y.clamp(0.0, (self.config.height - 1) as f32);
            let width = width.clamp(1.0, self.config.width as f32 - x);
            let height = height.clamp(1.0, self.config.height as f32 - y);
            pass.set_viewport(x, y, width, height, 0.0, 1.0);
            pass.set_scissor_rect(x as u32, y as u32, width as u32, height as u32);
            pass.set_pipeline(&self.background_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
            if let Some(m) = self.active_buffers() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, m.positions.slice(..));
                pass.set_vertex_buffer(1, m.normals.slice(..));
                pass.set_index_buffer(m.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..m.index_count, 0, 0..1);
            }
        }

        // egui pass.
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point,
        };
        for (id, deltas) in textures_delta.set.drain() {
            for delta in deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, id, &delta);
            }
        }
        let egui_cmds = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            paint_jobs,
            &screen,
        );
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut pass, paint_jobs, &screen);
        }
        self.queue.submit(
            egui_cmds
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        self.queue.present(frame);
        for id in textures_delta.free.drain() {
            self.egui_renderer.free_texture(&id);
        }
    }
}

fn create_depth(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}
