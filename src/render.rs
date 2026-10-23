//! wgpu renderer: a compute pass that refreshes normals of dirty chunks, one pipeline for the
//! sculpt mesh, and the egui overlay.

use crate::mesh::SculptMesh;
use crate::multires::StandIn;
use rayon::prelude::*;
use std::collections::HashMap;
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
    /// Map applied to the active subtool in the vertex shader, weighted per vertex by how
    /// free it is of the mask: the live preview of a Transpose move / scale / rotate (see
    /// `transform::Partial`). Centre (w: 1 while a preview is on); rotation axis and angle;
    /// translation and scale.
    /// Parked stand-ins ignore it.
    pub xf_center: [f32; 4],
    pub xf_rotation: [f32; 4],
    pub xf_move: [f32; 4],
}

/// Uniform of the normal passes (see `normals.wgsl`). Written once per frame that needs
/// normals, since the chunk counts follow the work list.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NormalParams {
    scale: f32,
    cap: f32,
    chunk_count: u32,
    flagged: u32,
    listed: u32,
    _pad: [u32; 3],
}

/// Fixed point for the normal accumulators: a face of typical size contributes about 2^18
/// per axis; one face is capped at 2^24 so the sum of up to 128 faces cannot overflow.
const NORMAL_TYPICAL: f32 = (1u32 << 18) as f32;
const NORMAL_CAP: f32 = (1u32 << 24) as f32;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GrabParams {
    delta: [f32; 3],
    count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AffineParams {
    model: [[f32; 4]; 4],
    count: u32,
    _pad: [u32; 3],
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

/// GPU copy of one subdivision level, plus a four-byte paint buffer per vertex.
/// Base geometry uses 52 bytes per vertex (positions 12, packed normals 4,
/// indices 24, normal accumulators 12) plus a few words per chunk.
struct MeshBuffers {
    positions: wgpu::Buffer,
    normals: wgpu::Buffer,
    colors: wgpu::Buffer,
    indices: wgpu::Buffer,
    /// Per-chunk flags for the normal passes (1 = recompute).
    chunk_flag: wgpu::Buffer,
    /// Chunk ids the normal passes visit: flagged chunks first, then their edge neighbours.
    work: wgpu::Buffer,
    normal_params: wgpu::Buffer,
    normal_bind_group: wgpu::BindGroup,
    index_count: u32,
    /// Fixed-point scale of the normal accumulators for this level.
    normal_scale: f32,
    /// CPU side of `chunk_flag`.
    dirty_flags: Vec<u32>,
    /// Flagged and listed chunks of the next normal passes.
    flagged: u32,
    listed: u32,
    /// Mask, one byte per vertex (read by the vertex shader as packed words).
    mask: wgpu::Buffer,
    mask_bind_group: wgpu::BindGroup,
    /// GPU memory held by this level (with `draw` and `groups`).
    bytes: u64,
    /// Visible triangles only, drawn and picked instead of `indices` while some are hidden
    /// (12 bytes per visible triangle), and the mesh's `vis_version` it was made for.
    draw: Option<(wgpu::Buffer, u32)>,
    vis_version: Option<u32>,
    /// Polygroup per drawn triangle for the PolyF pass (2 bytes per triangle), while shown.
    groups: Option<GroupBuffers>,
    /// What the buffers have room for (see [`Gfx::swap_tail`]).
    room: Room,
    // Kept alive for the bind group.
    _tables: [wgpu::Buffer; 4],
}

/// Vertices, triangles and chunks a level's buffers have room for.
#[derive(Clone, Copy, Debug, Default)]
pub struct Room {
    pub verts: usize,
    pub tris: usize,
    pub chunks: usize,
}

struct GroupBuffers {
    bind_group: wgpu::BindGroup,
    /// `(vis_version, group_version)` of the mesh it was made for.
    versions: (u32, u32),
    bytes: u64,
    _buffer: wgpu::Buffer,
}

/// Draw-only GPU copy of a parked subtool, plus four-byte vertex paint.
/// Base geometry uses 40 bytes per vertex (positions 12, baked
/// normals 4, indices 24), no normal pass, no chunk tables.
struct StandInBuffers {
    positions: wgpu::Buffer,
    normals: wgpu::Buffer,
    colors: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    bytes: u64,
}

/// Target of the id pass used to pick a subtool under the cursor.
struct PickTarget {
    view: wgpu::TextureView,
    texture: wgpu::Texture,
    size: (u32, u32),
}

/// Levels that are not shown keep their GPU copies while they total at most this much;
/// beyond it the largest ones are dropped and re-uploaded on their next visit. A
/// 61M-vertex level alone is 3.2 GB, so this is what lets it fit next to the lower levels
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
    /// Polygroup colours (PolyF); `None` when the GPU has no primitive index.
    group_pipeline: Option<wgpu::RenderPipeline>,
    group_bgl: wgpu::BindGroupLayout,
    background_pipeline: wgpu::RenderPipeline,
    pick_pipeline: wgpu::RenderPipeline,
    pick_target: Option<PickTarget>,
    pick_readback: wgpu::Buffer,
    /// Parked subtools, by subtool id.
    stand_ins: HashMap<u64, StandInBuffers>,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    mask_bgl: wgpu::BindGroupLayout,
    /// Bound for stand-ins, which have no mask.
    no_mask: wgpu::BindGroup,
    normal_own: wgpu::ComputePipeline,
    normal_border: wgpu::ComputePipeline,
    normal_normalize: wgpu::ComputePipeline,
    normal_bgl: wgpu::BindGroupLayout,
    grab_pipeline: wgpu::ComputePipeline,
    grab_bgl: wgpu::BindGroupLayout,
    grab_params: wgpu::Buffer,
    grab: Option<GpuGrab>,
    affine_pipeline: wgpu::ComputePipeline,
    affine_bgl: wgpu::BindGroupLayout,
    affine_params: wgpu::Buffer,
    /// Map queued for the positions of a level by [`Gfx::transform_level`], run at the start
    /// of the next frame: (level, matrix).
    pending_affine: Option<(usize, glam::Mat4)>,
    /// One entry per subdivision level, uploaded the first time the level is shown so
    /// stepping between levels is a pointer swap.
    levels: Vec<Option<MeshBuffers>>,
    active: usize,
    needs_normals: bool,
    pub egui_renderer: egui_wgpu::Renderer,
    pub adapter_name: String,
    /// `Multires::generation` of the stack whose levels are uploaded.
    pub generation: u64,
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

        // The polygroup pass reads the triangle index in the fragment shader; only that
        // pipeline uses it.
        let primitive_index = adapter.features().contains(wgpu::Features::PRIMITIVE_INDEX);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("klay"),
            required_features: if primitive_index {
                wgpu::Features::PRIMITIVE_INDEX
            } else {
                wgpu::Features::empty()
            },
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
            label: Some("uniform layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        // The mesh pipeline also reads the mask in the vertex shader.
        let mask_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // The PolyF pass places its triangle's corners the way the vertex shader does.
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh layout"),
            bind_group_layouts: &[Some(&bgl), Some(&mask_bgl)],
            immediate_size: 0,
        });
        let no_mask_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("no mask"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let no_mask = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("no mask bg"),
            layout: &mask_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: no_mask_buf.as_entire_binding(),
            }],
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
        let color_attr = [wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Unorm8x4,
            offset: 0,
            shader_location: 2,
        }];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh pipeline"),
            layout: Some(&mesh_layout),
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
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 4,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &color_attr,
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

        // PolyF: the polygroup of each drawn triangle, the drawn index buffer and the
        // positions, so the fragment shader can find its triangle's edges.
        let fragment_storage = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let group_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("polygroup bgl"),
            entries: &[fragment_storage(0), fragment_storage(1), fragment_storage(2)],
        });
        let group_pipeline = primitive_index.then(|| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("polygroup shader"),
                source: wgpu::ShaderSource::Wgsl(
                    format!(
                        "enable primitive_index;\n{}\n{}",
                        include_str!("shader.wgsl"),
                        include_str!("groups.wgsl")
                    )
                    .into(),
                ),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("polygroup layout"),
                // The mesh pipeline's uniforms and mask, then the polygroups.
                bind_group_layouts: &[Some(&bgl), Some(&mask_bgl), Some(&group_bgl)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("polygroup pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
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
                        Some(wgpu::VertexBufferLayout {
                            array_stride: 4,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &color_attr,
                        }),
                    ],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
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
                    module: &module,
                    entry_point: Some("fs_groups"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
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

        // Id pass for picking: positions only, writes the instance index as the id.
        let pick_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pick pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_pick"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &pos_attr,
                })],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
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
                entry_point: Some("fs_pick"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Uint,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let pick_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pick readback"),
            size: 256,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
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
                storage(6, true),
                storage(7, false),
                storage(8, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
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
        let normal_pass = |label: &str, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&normal_layout),
                module: &normal_shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let normal_own = normal_pass("normals own", "cs_own");
        let normal_border = normal_pass("normals border", "cs_border");
        let normal_normalize = normal_pass("normals normalize", "cs_normalize");
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
        // --- Affine map of a whole level (Transpose release) ------------------------------
        let affine_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("affine"),
            source: wgpu::ShaderSource::Wgsl(include_str!("affine.wgsl").into()),
        });
        let affine_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("affine bgl"),
            entries: &[
                storage(0, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
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
        let affine_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("affine layout"),
            bind_group_layouts: &[Some(&affine_bgl)],
            immediate_size: 0,
        });
        let affine_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("affine pipeline"),
            layout: Some(&affine_layout),
            module: &affine_shader,
            entry_point: Some("cs_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let affine_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("affine params"),
            size: std::mem::size_of::<AffineParams>() as u64,
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
            group_pipeline,
            group_bgl,
            uniform_buf,
            background_pipeline,
            pick_pipeline,
            pick_target: None,
            pick_readback,
            stand_ins: HashMap::new(),
            bind_group,
            mask_bgl,
            no_mask,
            normal_own,
            normal_border,
            normal_normalize,
            normal_bgl,
            grab_pipeline,
            grab_bgl,
            grab_params,
            grab: None,
            affine_pipeline,
            affine_bgl,
            affine_params,
            pending_affine: None,
            levels: Vec::new(),
            active: 0,
            needs_normals: false,
            egui_renderer,
            adapter_name,
            generation: 0,
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

    /// Upload the draw-only copy of a parked subtool. Returns false when it does not fit;
    /// the subtool is then not drawn while parked.
    pub fn upload_stand_in(&mut self, id: u64, s: &StandIn) -> bool {
        use wgpu::BufferUsages as U;
        self.stand_ins.remove(&id);
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let positions = self.upload("stand-in positions", bytemuck::cast_slice(&s.positions), U::VERTEX);
        let normals = self.upload("stand-in normals", bytemuck::cast_slice(&s.normals), U::VERTEX);
        let colors = if s.colors.is_empty() {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unpainted stand-in"),size:(s.positions.len()*4).max(4) as u64,
                usage:U::VERTEX,mapped_at_creation:false,
            })
        } else {self.upload("stand-in paint",bytemuck::cast_slice(&s.colors),U::VERTEX)};
        let indices = self.upload("stand-in indices", bytemuck::cast_slice(&s.indices), U::INDEX);
        let bytes = positions.size() + normals.size() + colors.size() + indices.size();
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("gpu: a stand-in of {} MB did not fit: {e}", bytes / 1_000_000);
            return false;
        }
        self.stand_ins.insert(
            id,
            StandInBuffers {
                positions,
                normals,
                colors,
                indices,
                index_count: s.indices.len() as u32,
                bytes,
            },
        );
        true
    }

    /// Triangles drawn for the active level (fewer than it has while some are hidden).
    pub fn drawn_triangles(&self) -> usize {
        self.active_buffers().map_or(0, |m| m.drawn().1 as usize / 3)
    }

    /// GPU bytes of the active level's visible-index and polygroup buffers.
    pub fn marks_bytes(&self) -> (u64, u64) {
        self.active_buffers().map_or((0, 0), |m| {
            (
                m.draw.as_ref().map_or(0, |(b, _)| b.size()),
                m.groups.as_ref().map_or(0, |g| g.bytes),
            )
        })
    }

    /// Whether polygroup colours can be shown (the GPU has primitive index support).
    pub fn group_overlay_supported(&self) -> bool {
        self.group_pipeline.is_some()
    }

    pub fn drop_stand_in(&mut self, id: u64) {
        self.stand_ins.remove(&id);
    }

    /// GPU memory held by the stand-ins of parked subtools.
    pub fn stand_in_bytes(&self) -> u64 {
        self.stand_ins.values().map(|s| s.bytes).sum()
    }

    /// Id of what is under pixel `(x, y)` of the last rendered frame: `active_id` for the
    /// active subtool, the given id for a parked one, 0 for the background. One pixel is
    /// rasterised, so this costs a draw call per subtool and a short wait.
    pub fn pick(&mut self, x: u32, y: u32, viewport: [f32; 4], active_id: u32, parked: &[(u64, u32)]) -> u32 {
        let (w, h) = (self.config.width, self.config.height);
        if x >= w || y >= h {
            return 0;
        }
        if self.pick_target.as_ref().is_none_or(|t| t.size != (w, h)) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pick target"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Uint,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.pick_target = Some(PickTarget {
                view,
                texture,
                size: (w, h),
            });
        }
        let target = self.pick_target.as_ref().unwrap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pick") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pick"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
            let [vx, vy, vw, vh] = viewport;
            let vx = vx.clamp(0.0, (w - 1) as f32);
            let vy = vy.clamp(0.0, (h - 1) as f32);
            pass.set_viewport(vx, vy, vw.clamp(1.0, w as f32 - vx), vh.clamp(1.0, h as f32 - vy), 0.0, 1.0);
            pass.set_scissor_rect(x, y, 1, 1);
            pass.set_pipeline(&self.pick_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            if let Some(m) = self.levels.get(self.active).and_then(Option::as_ref) {
                let (indices, count) = m.drawn();
                pass.set_vertex_buffer(0, m.positions.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..count, 0, active_id..active_id + 1);
            }
            for &(key, id) in parked {
                if let Some(s) = self.stand_ins.get(&key) {
                    pass.set_vertex_buffer(0, s.positions.slice(..));
                    pass.set_index_buffer(s.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..s.index_count, 0, id..id + 1);
                }
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.pick_readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        self.pick_readback.map_async(wgpu::MapMode::Read, .., |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let id = match self.pick_readback.get_mapped_range(..4) {
            Ok(view) => u32::from_le_bytes([view[0], view[1], view[2], view[3]]),
            Err(_) => 0,
        };
        self.pick_readback.unmap();
        id
    }

    /// Read the shown level's positions back from the GPU (self-tests).
    pub fn read_positions(&self) -> Option<Vec<glam::Vec3>> {
        let b = self.active_buffers()?;
        self.read_buffer(&b.positions)
    }

    /// Read the shown level's mask back from the GPU, padded to whole words (self-tests).
    pub fn read_mask(&self) -> Option<Vec<u8>> {
        let b = self.active_buffers()?;
        self.read_buffer(&b.mask)
    }

    /// Read premultiplied vertex paint for native pipeline verification.
    pub fn read_colors(&self) -> Option<Vec<[u8; 4]>> {
        self.read_buffer(&self.active_buffers()?.colors)
    }

    fn read_buffer<T: bytemuck::Pod>(&self, buffer: &wgpu::Buffer) -> Option<Vec<T>> {
        let size = buffer.size();
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("positions readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        encoder.copy_buffer_to_buffer(buffer, 0, &readback, 0, size);
        self.queue.submit(std::iter::once(encoder.finish()));
        readback.map_async(wgpu::MapMode::Read, .., |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let out = match readback.get_mapped_range(..) {
            Ok(view) => bytemuck::pod_collect_to_vec(&view[..]),
            Err(_) => return None,
        };
        readback.unmap();
        Some(out)
    }

    /// Forget every uploaded level (the document was replaced).
    pub fn clear_levels(&mut self) {
        self.levels.clear();
        self.active = 0;
        self.grab = None;
        self.pending_affine = None;
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
        self.upload_in(label, data, data.len(), usage)
    }

    /// [`upload`](Self::upload) into a buffer of at least `size` bytes.
    fn upload_in(&self, label: &str, data: &[u8], size: usize, usage: wgpu::BufferUsages) -> wgpu::Buffer {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: data.len().max(size).max(4).next_multiple_of(4) as u64,
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
        self.upload_level_with_room(mesh, level, Room::default())
    }

    /// [`upload_level`](Self::upload_level) with buffers big enough for `room` (vertices,
    /// triangles, chunks), so a part can later be added in place (see [`swap_tail`](Self::swap_tail)).
    pub fn upload_level_with_room(&mut self, mesh: &mut SculptMesh, level: usize, room: Room) -> bool {
        let previous = self.active;
        if self.levels.len() <= level {
            self.levels.resize_with(level + 1, || None);
        }
        self.levels[level] = None;
        if self.pending_affine.is_some_and(|(l, _)| l == level) {
            self.pending_affine = None;
        }
        self.active = level;
        self.trim_cache();
        for attempt in 0..2 {
            match self.create_level(mesh, room) {
                Some(buffers) => {
                    log::info!(
                        "gpu: level {level} uploaded, {} MB ({} bytes per vertex)",
                        buffers.bytes / 1_000_000,
                        buffers.bytes / mesh.vertex_count().max(1) as u64
                    );
                    self.levels[level] = Some(buffers);
                    for ch in &mut mesh.chunks {
                        ch.dirty_pos = false;
                        ch.dirty_nrm = true;
                        ch.dirty_mask = false;
                        ch.dirty_color = false;
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
    fn create_level(&self, mesh: &SculptMesh, room: Room) -> Option<MeshBuffers> {
        use wgpu::BufferUsages as U;
        let room = Room {
            verts: room.verts.max(mesh.vertex_count()),
            tris: room.tris.max(mesh.triangle_count()),
            chunks: room.chunks.max(mesh.chunks.len()),
        };
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let positions = self.upload_in(
            "positions",
            bytemuck::cast_slice(&mesh.positions),
            room.verts * 12,
            // COPY_SRC: read back by `--selftest-transpose`.
            U::VERTEX | U::STORAGE | U::COPY_SRC,
        );
        let indices = self.upload_in(
            "indices",
            bytemuck::cast_slice(&mesh.indices),
            room.tris * 12,
            U::INDEX | U::STORAGE,
        );
        let chunk_count = mesh.chunks.len();
        let mut chunk_verts = Vec::with_capacity(chunk_count + 1);
        let mut chunk_tris = Vec::with_capacity(chunk_count + 1);
        let mut chunk_border = Vec::with_capacity(chunk_count);
        for ch in &mesh.chunks {
            chunk_verts.push(ch.vert_start);
            chunk_tris.push(ch.tri_start);
            chunk_border.push(ch.tri_border);
        }
        chunk_verts.push(mesh.vertex_count() as u32);
        chunk_tris.push(mesh.triangle_count() as u32);
        let tables = (room.chunks + 1) * 4;
        let chunk_verts = self.upload_in("chunk_verts", bytemuck::cast_slice(&chunk_verts), tables, U::STORAGE);
        let chunk_tris = self.upload_in("chunk_tris", bytemuck::cast_slice(&chunk_tris), tables, U::STORAGE);
        let chunk_border = self.upload_in("chunk_border", bytemuck::cast_slice(&chunk_border), tables, U::STORAGE);
        let plain = |label: &str, size: usize, usage: U| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size.max(4) as u64,
                usage,
                mapped_at_creation: false,
            })
        };
        let normals = plain("normals", room.verts * 4, U::VERTEX | U::STORAGE);
        let colors=if mesh.colors.is_empty() {
            plain("paint",room.verts*4,U::VERTEX|U::COPY_DST|U::COPY_SRC)
        } else {self.upload_in("paint",bytemuck::cast_slice(&mesh.colors),room.verts*4,U::VERTEX|U::COPY_SRC)};
        let acc = plain("normal accumulators", room.verts * 12, U::STORAGE);
        let chunk_flag = plain("chunk_flag", room.chunks * 4, U::STORAGE | U::COPY_DST);
        let work = plain("normal work list", room.chunks * 4, U::STORAGE | U::COPY_DST);
        let normal_params = plain(
            "normal params",
            std::mem::size_of::<NormalParams>(),
            U::UNIFORM | U::COPY_DST,
        );
        // Padded to whole words; zero (nothing masked) unless the mesh has a mask.
        let mask = if mesh.mask.is_empty() {
            plain("mask", room.verts.div_ceil(4) * 4, U::STORAGE | U::COPY_DST | U::COPY_SRC)
        } else {
            let mut padded = mesh.mask.clone();
            padded.resize(room.verts.div_ceil(4) * 4, 0);
            // COPY_SRC: read back by `--selftest-mask`.
            self.upload("mask", &padded, U::STORAGE | U::COPY_SRC)
        };
        let bytes = [
            &mask,
            &colors,
            &positions,
            &indices,
            &chunk_verts,
            &chunk_tris,
            &chunk_border,
            &normals,
            &acc,
            &chunk_flag,
            &work,
        ]
        .iter()
        .map(|b| b.size())
        .sum();
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("gpu: a level of {} MB did not fit: {e}", bytes / 1_000_000);
            drop((
                mask,
                positions,
                indices,
                chunk_verts,
                chunk_tris,
                chunk_border,
                normals,
                colors,
                acc,
                chunk_flag,
                work,
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
                    resource: chunk_verts.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: chunk_tris.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: chunk_border.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: chunk_flag.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: work.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: acc.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: normals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: normal_params.as_entire_binding(),
                },
            ],
        });
        let mask_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask bg"),
            layout: &self.mask_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: mask.as_entire_binding(),
            }],
        });
        Some(MeshBuffers {
            mask,
            mask_bind_group,
            positions,
            normals,
            colors,
            indices,
            chunk_flag,
            work,
            normal_params,
            normal_bind_group,
            index_count: mesh.indices.len() as u32,
            normal_scale: normal_scale(mesh),
            dirty_flags: vec![0; chunk_count],
            flagged: 0,
            listed: 0,
            bytes,
            draw: None,
            vis_version: None,
            groups: None,
            room,
            _tables: [chunk_verts, chunk_tris, chunk_border, acc],
        })
    }

    /// Show the active level again after everything after `keep` in it changed (a
    /// CurveTube added, bent or removed), writing only that part when its buffers have room
    /// (see [`upload_level_with_room`](Self::upload_level_with_room)). False when they do
    /// not: the caller uploads the level afresh.
    pub fn swap_tail(&mut self, mesh: &mut SculptMesh, keep: crate::mesh::Extent) -> bool {
        let Some(b) = self.levels.get_mut(self.active).and_then(Option::as_mut) else {
            return false;
        };
        let (n, t, c) = (mesh.vertex_count(), mesh.triangle_count(), mesh.chunks.len());
        if n > b.room.verts || t > b.room.tris || c > b.room.chunks || keep.chunks > c {
            return false;
        }
        let (kv, kt) = (keep.verts, keep.tris);
        let q = &self.queue;
        if n > kv {
            q.write_buffer(&b.positions, (kv * 12) as u64, bytemuck::cast_slice(&mesh.positions[kv..]));
            if mesh.colors.is_empty() {
                q.write_buffer(&b.colors, (kv * 4) as u64, &vec![0u8; (n - kv) * 4]);
            } else {
                q.write_buffer(&b.colors, (kv * 4) as u64, bytemuck::cast_slice(&mesh.colors[kv..]));
            }
            // One byte per vertex in whole words: from the word holding the first new one.
            let (from, to) = (kv / 4 * 4, n.div_ceil(4) * 4);
            let mut words = if mesh.mask.is_empty() { Vec::new() } else { mesh.mask[from..].to_vec() };
            words.resize(to - from, 0);
            q.write_buffer(&b.mask, from as u64, &words);
        }
        if t > kt {
            q.write_buffer(&b.indices, (kt * 12) as u64, bytemuck::cast_slice(&mesh.indices[3 * kt..]));
        }
        let mut verts: Vec<u32> = mesh.chunks.iter().map(|ch| ch.vert_start).collect();
        let mut tris: Vec<u32> = mesh.chunks.iter().map(|ch| ch.tri_start).collect();
        let border: Vec<u32> = mesh.chunks.iter().map(|ch| ch.tri_border).collect();
        verts.push(n as u32);
        tris.push(t as u32);
        let [chunk_verts, chunk_tris, chunk_border, _] = &b._tables;
        q.write_buffer(chunk_verts, 0, bytemuck::cast_slice(&verts));
        q.write_buffer(chunk_tris, 0, bytemuck::cast_slice(&tris));
        if !border.is_empty() {
            q.write_buffer(chunk_border, 0, bytemuck::cast_slice(&border));
        }
        b.index_count = (3 * t) as u32;
        b.dirty_flags.resize(c, 0);
        // Rebuilt from the mesh when next needed.
        b.bytes -= b.draw.as_ref().map_or(0, |(d, _)| d.size()) + b.groups.as_ref().map_or(0, |g| g.bytes);
        b.draw = None;
        b.vis_version = None;
        b.groups = None;
        for ch in &mut mesh.chunks[keep.chunks..] {
            ch.dirty_pos = false;
            ch.dirty_mask = false;
            ch.dirty_color = false;
            ch.dirty_nrm = true;
        }
        true
    }

    /// Bring the active level's visible-triangle index buffer and, while `want_groups`, its
    /// polygroup buffer in line with the mesh. Both exist only while needed, so a model
    /// with nothing hidden and polygroups off holds and draws exactly what it did before.
    fn sync_marks(&mut self, mesh: &SculptMesh, want_groups: bool) {
        let can_group = self.group_pipeline.is_some();
        let Some(m) = self.levels.get(self.active).and_then(Option::as_ref) else {
            return;
        };
        let vis_stale = m.vis_version != Some(mesh.vis_version);
        let versions = (mesh.vis_version, mesh.group_version);
        let groups_stale = want_groups && can_group && m.groups.as_ref().is_none_or(|g| g.versions != versions);
        let drop_groups = !(want_groups && can_group) && m.groups.is_some();
        if !vis_stale && !groups_stale && !drop_groups {
            return;
        }
        let draw = vis_stale.then(|| {
            mesh.visible_indices().map(|ix| {
                // STORAGE: PolyF reads the corners of the drawn triangles from it.
                let usage = wgpu::BufferUsages::INDEX | wgpu::BufferUsages::STORAGE;
                (self.upload("visible indices", bytemuck::cast_slice(&ix), usage), ix.len() as u32)
            })
        });
        if let Some(draw) = draw {
            let m = self.levels[self.active].as_mut().unwrap();
            m.bytes -= m.draw.as_ref().map_or(0, |(b, _)| b.size());
            m.bytes += draw.as_ref().map_or(0, |(b, _)| b.size());
            m.draw = draw;
            m.vis_version = Some(mesh.vis_version);
        }
        // Made after the visible indices, which it binds.
        let groups = groups_stale.then(|| {
            let m = self.levels[self.active].as_ref().unwrap();
            let packed = mesh.drawn_groups_packed();
            let buffer = self.upload("polygroups", bytemuck::cast_slice(&packed), wgpu::BufferUsages::STORAGE);
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("polygroup bg"),
                layout: &self.group_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: m.drawn().0.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: m.positions.as_entire_binding(),
                    },
                ],
            });
            GroupBuffers {
                bind_group,
                versions,
                bytes: buffer.size(),
                _buffer: buffer,
            }
        });
        let m = self.levels[self.active].as_mut().unwrap();
        if groups.is_some() || drop_groups {
            m.bytes -= m.groups.as_ref().map_or(0, |g| g.bytes);
            m.bytes += groups.as_ref().map_or(0, |g| g.bytes);
            m.groups = groups;
        }
    }

    /// Start a grab on the GPU: upload the selection, its original positions and weights once.
    pub fn begin_gpu_grab(&mut self, setup: crate::sculpt::GpuGrabSetup<'_>, flags: Vec<u32>) {
        let Some(m) = self.active_buffers() else {
            return;
        };
        // wgpu refuses to bind empty buffers.
        if setup.verts.is_empty() {
            return;
        }
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

    /// The CPU just mapped every level of the active subtool by `map`. Instead of uploading
    /// the shown level again, map its GPU positions in place on the next frame and mark its
    /// chunks clean; normals are recomputed only when the map turns them. `mesh` is the
    /// shown level, and must have had no changes waiting for upload before the map (the
    /// caller checks with [`SculptMesh`] chunk flags). Every cached level's normal
    /// accumulator scale follows the change in face area.
    pub fn transform_level(&mut self, mesh: &mut SculptMesh, map: &crate::transform::Affine) {
        for b in self.levels.iter_mut().flatten() {
            b.normal_scale /= map.scale * map.scale;
        }
        if self.active_buffers().is_none() {
            return;
        }
        let level = self.active;
        let m = map.matrix();
        self.pending_affine = Some(match self.pending_affine.take() {
            Some((l, earlier)) if l == level => (level, m * earlier),
            _ => (level, m),
        });
        let turns = map.rotation != glam::Quat::IDENTITY;
        for ch in &mut mesh.chunks {
            ch.dirty_pos = false;
            ch.dirty_nrm = turns;
        }
    }

    /// Normal accumulator scale after a map that did not go through
    /// [`transform_level`](Self::transform_level) (the shown level is uploaded again).
    pub fn rescale_normals(&mut self, scale: f32) {
        for b in self.levels.iter_mut().flatten() {
            b.normal_scale /= scale * scale;
        }
    }

    pub fn gpu_grab_active(&self) -> bool {
        self.grab.is_some()
    }

    /// Re-upload positions of chunks whose vertices moved (runs of consecutive dirty chunks
    /// are merged into one copy each) and flag chunks whose normals must be recomputed.
    pub fn upload_dirty(&mut self, mesh: &mut SculptMesh, want_groups: bool) {
        self.sync_marks(mesh, want_groups);
        let Some(buffers) = self.levels.get_mut(self.active).and_then(Option::as_mut) else {
            return;
        };
        debug_assert_eq!(buffers.dirty_flags.len(), mesh.chunks.len());
        let mut count = 0;
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut normal_chunks = 0;
        for (i, ch) in mesh.chunks.iter_mut().enumerate() {
            if std::mem::take(&mut ch.dirty_color) {
                let (a,b)=ch.range();
                if mesh.colors.is_empty() {
                    self.queue.write_buffer(&buffers.colors,(a*4) as u64,&vec![0;(b-a)*4]);
                } else {
                    self.queue.write_buffer(&buffers.colors,(a*4) as u64,bytemuck::cast_slice(&mesh.colors[a..b]));
                }
            }
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
        // Mask: runs of chunks whose values changed, widened to whole words.
        let mut mask_runs: Vec<(usize, usize)> = Vec::new();
        for ch in mesh.chunks.iter_mut() {
            if std::mem::take(&mut ch.dirty_mask) {
                let (s, e) = ch.range();
                let (s, e) = (s / 4 * 4, e.div_ceil(4) * 4);
                match mask_runs.last_mut() {
                    Some(last) if last.1 >= s => last.1 = last.1.max(e),
                    _ => mask_runs.push((s, e)),
                }
            }
        }
        let n = mesh.vertex_count();
        for (s, e) in mask_runs {
            let mut data = vec![0u8; e - s];
            if !mesh.mask.is_empty() {
                let end = e.min(n);
                data[..end - s].copy_from_slice(&mesh.mask[s..end]);
            }
            self.queue.write_buffer(&buffers.mask, s as u64, &data);
            bytes += data.len();
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
            // Work list: the flagged chunks, then every chunk sharing an edge with one of
            // them (their triangles touch flagged vertices).
            let flags = &buffers.dirty_flags;
            let mut work: Vec<u32> = (0..flags.len() as u32).filter(|&c| flags[c as usize] != 0).collect();
            let flagged = work.len();
            let mut listed = flags.clone();
            for k in 0..flagged {
                for &nb in &mesh.chunks[work[k] as usize].neighbors {
                    if listed[nb as usize] == 0 {
                        listed[nb as usize] = 1;
                        work.push(nb);
                    }
                }
            }
            buffers.flagged = flagged as u32;
            buffers.listed = work.len() as u32;
            if flagged > 64 {
                log::debug!(
                    "normals work list: {flagged} flagged + {} neighbour chunks of {}",
                    work.len() - flagged,
                    flags.len()
                );
            }
            self.queue.write_buffer(&buffers.chunk_flag, 0, bytemuck::cast_slice(flags));
            self.queue.write_buffer(&buffers.work, 0, bytemuck::cast_slice(&work));
            self.queue.write_buffer(
                &buffers.normal_params,
                0,
                bytemuck::bytes_of(&NormalParams {
                    scale: buffers.normal_scale,
                    cap: NORMAL_CAP,
                    chunk_count: flags.len() as u32,
                    flagged: buffers.flagged,
                    listed: buffers.listed,
                    _pad: [0; 3],
                }),
            );
        }
        self.needs_normals = normal_chunks > 0;
        self.last_normal_chunks = normal_chunks;
        self.last_upload_bytes = bytes;
        self.last_upload_chunks = count;
    }

    /// Draw a frame: the active subtool, the stand-ins listed in `parked` (dimmed, no brush
    /// ring), then the interface.
    pub fn render(
        &mut self,
        uniforms: &Uniforms,
        parked: &[u64],
        polyframe: bool,
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

        // Transpose release: map the positions of a level in place.
        if let Some((level, m)) = self.pending_affine.take() {
            if let Some(b) = self.levels.get(level).and_then(Option::as_ref) {
                let count = (b.positions.size() / 12) as u32;
                self.queue.write_buffer(
                    &self.affine_params,
                    0,
                    bytemuck::bytes_of(&AffineParams {
                        model: m.to_cols_array_2d(),
                        count,
                        _pad: [0; 3],
                    }),
                );
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("affine bg"),
                    layout: &self.affine_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: b.positions.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: self.affine_params.as_entire_binding(),
                        },
                    ],
                });
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("affine"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.affine_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                let groups = count.div_ceil(256);
                pass.dispatch_workgroups(groups.min(65535), groups.div_ceil(65535), 1);
            }
        }

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

        // Normal recompute for dirty chunks: own-vertex sums, cross-chunk sums, normalise.
        let needs_normals = std::mem::take(&mut self.needs_normals);
        if let (Some(m), true) = (self.active_buffers(), needs_normals) {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("normals"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &m.normal_bind_group, &[]);
            for (pipeline, chunks) in [
                (&self.normal_own, m.flagged),
                (&self.normal_border, m.listed),
                (&self.normal_normalize, m.flagged),
            ] {
                pass.set_pipeline(pipeline);
                pass.dispatch_workgroups(chunks.min(65535), chunks.div_ceil(65535), 1);
            }
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
                match (&self.group_pipeline, &m.groups, polyframe) {
                    (Some(pipeline), Some(g), true) => {
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(2, &g.bind_group, &[]);
                    }
                    _ => pass.set_pipeline(&self.pipeline),
                }
                let (indices, count) = m.drawn();
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_bind_group(1, &m.mask_bind_group, &[]);
                pass.set_vertex_buffer(0, m.positions.slice(..));
                pass.set_vertex_buffer(1, m.normals.slice(..));
                pass.set_vertex_buffer(2, m.colors.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..count, 0, 0..1);
            }
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_bind_group(1, &self.no_mask, &[]);
            for key in parked {
                if let Some(s) = self.stand_ins.get(key) {
                    pass.set_vertex_buffer(0, s.positions.slice(..));
                    pass.set_vertex_buffer(1, s.normals.slice(..));
                    pass.set_vertex_buffer(2, s.colors.slice(..));
                    pass.set_index_buffer(s.indices.slice(..), wgpu::IndexFormat::Uint32);
                    // Instance 1 marks a parked subtool for the shader.
                    pass.draw_indexed(0..s.index_count, 0, 1..2);
                }
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

impl MeshBuffers {
    /// The index buffer to draw and pick with, and its index count.
    fn drawn(&self) -> (&wgpu::Buffer, u32) {
        match &self.draw {
            Some((b, n)) => (b, *n),
            None => (&self.indices, self.index_count),
        }
    }
}

/// Fixed-point scale of the normal accumulators for a level: a face of mean area lands at
/// `NORMAL_TYPICAL`. The mean is taken from an even sample of the triangles.
fn normal_scale(mesh: &SculptMesh) -> f32 {
    let tris = mesh.triangle_count();
    let step = (tris / 65536).max(1);
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for t in (0..tris).step_by(step) {
        let i = &mesh.indices[3 * t..3 * t + 3];
        let a = mesh.positions[i[0] as usize];
        let b = mesh.positions[i[1] as usize];
        let c = mesh.positions[i[2] as usize];
        sum += (b - a).cross(c - a).length() as f64;
        n += 1;
    }
    let mean = if n > 0 { sum / n as f64 } else { 0.0 };
    if mean > 0.0 { NORMAL_TYPICAL / mean as f32 } else { 1.0 }
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
