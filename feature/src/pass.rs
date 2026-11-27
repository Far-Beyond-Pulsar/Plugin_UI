//! The UI pass: one pipeline, instanced quads, over the final image at output
//! resolution. [`UiLayerKind::Screen`] draws the screen layer in one draw
//! call; [`UiLayerKind::World`] draws one range per world widget, back to
//! front, each depth-tested in the shader against the scene.

use std::sync::{Arc, Mutex};

use helio_core::graph::ResourceBuilder;
use helio_core::{PassContext, PrepareContext, RenderPass, Result as HelioResult};
use helio_ui_api::{UiFrame, UiPlacement, UiPrimitive, UI_FRAME};

use crate::heap::Heap;

const PRIMITIVE_BYTES: u64 = std::mem::size_of::<UiPrimitive>() as u64;
const FIRST_PRIMITIVES: usize = 256;

/// The GPU copy of the glyph atlas, one for both passes of a graph.
#[derive(Clone)]
pub struct SharedAtlas(Arc<AtlasTexture>);

pub struct AtlasTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// The atlas version the texture holds.
    seen: Mutex<u64>,
}

impl SharedAtlas {
    pub fn new(device: &wgpu::Device, side: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("UI Glyph Atlas"),
            size: wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("UI Atlas Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self(Arc::new(AtlasTexture {
            texture,
            view,
            sampler,
            seen: Mutex::new(0),
        }))
    }

    fn sync(&self, queue: &wgpu::Queue, frame: &UiFrame) {
        let side = frame.atlas.size();
        let mut seen = self
            .0
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *seen = frame
            .atlas
            .changes_since(*seen, &mut |x, y, region, texels| {
                let bytes = ((region - 1) * side + region) as usize * 4;
                helio_core::upload::write_texture(
                    queue,
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.0.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x, y, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &texels[..bytes],
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(side * 4),
                        rows_per_image: Some(region),
                    },
                    wgpu::Extent3d {
                        width: region,
                        height: region,
                        depth_or_array_layers: 1,
                    },
                );
            });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiLayerKind {
    Screen,
    World,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    output: [f32; 2],
    linear_output: u32,
    pad: u32,
}

struct WorldBindings {
    layout: wgpu::BindGroupLayout,
    placements: wgpu::Buffer,
    capacity: usize,
    revision: Option<u64>,
    centres: Vec<[f32; 3]>,
    /// The bind group and the depth view and camera buffer it was made for.
    group: Option<(wgpu::TextureView, wgpu::Buffer, wgpu::BindGroup)>,
}

/// See the module doc.
pub struct UiPass {
    kind: UiLayerKind,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    textures_layout: wgpu::BindGroupLayout,
    material_binding: helio_mats::MaterialBindingConfig,
    atlas: SharedAtlas,
    globals: wgpu::Buffer,
    shown_globals: Option<Globals>,
    linear_output: bool,
    primitives: wgpu::Buffer,
    capacity: usize,
    heap: Heap,
    group: Option<wgpu::BindGroup>,
    textures: Option<(u64, wgpu::BindGroup)>,
    /// Bound while no materials plugin publishes the texture table: text and
    /// shapes still draw.
    stand_in: helio_mats::MaterialStandIn,
    world: Option<WorldBindings>,
    /// World runs in draw order: `(squared distance, run index)`.
    order: Vec<(f32, u32)>,
    visible: bool,
}

fn primitive_buffer(device: &wgpu::Device, primitives: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("UI Primitives"),
        size: primitives as u64 * PRIMITIVE_BYTES,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn placement_buffer(device: &wgpu::Device, rows: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("UI Placements"),
        size: (rows * std::mem::size_of::<UiPlacement>()) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn storage_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// The shader of `kind` for a device's texture table.
pub fn shader_source(kind: UiLayerKind, binding: helio_mats::MaterialBindingConfig) -> String {
    let common = include_str!("../shaders/ui_common.wgsl");
    let body = match kind {
        UiLayerKind::Screen => include_str!("../shaders/ui_screen.wgsl"),
        UiLayerKind::World => include_str!("../shaders/ui_world.wgsl"),
    };
    let source = format!("{common}\n{body}");
    let source = if binding.uses_binding_arrays() {
        source.replace(
            "binding_array<texture_2d<f32>, 256>",
            &format!("binding_array<texture_2d<f32>, {}>", binding.max_textures),
        )
    } else {
        helio_mats::apply_webgpu_decal_bindings(&source, binding.max_textures)
    };
    // The rows and, for the world, the frame's camera come first.
    let prelude = match kind {
        UiLayerKind::Screen => "",
        UiLayerKind::World => "//!use helio_prelude\n",
    };
    let enable = if binding.uses_binding_arrays() {
        "enable wgpu_binding_array;\n"
    } else {
        ""
    };
    format!("{enable}{prelude}{}\n{source}", helio_ui_api::UI_WGSL)
}

impl UiPass {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        kind: UiLayerKind,
        target: wgpu::TextureFormat,
        atlas: SharedAtlas,
    ) -> Self {
        let material_binding = helio_mats::MaterialBindingConfig::for_device(device);
        let source = shader_source(kind, material_binding);
        let label = match kind {
            UiLayerKind::Screen => "UI Screen Shader",
            UiLayerKind::World => "UI World Shader",
        };
        let shader = helio_core::shader::module(device, label, source.as_str());
        let both = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("UI Layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: both,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage_entry(1, both),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let mut entries = Vec::new();
        material_binding.append_layout_entries(&mut entries, 0, wgpu::ShaderStages::FRAGMENT);
        let textures_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("UI Textures"),
            entries: &entries,
        });
        let world = (kind == UiLayerKind::World).then(|| WorldBindings {
            layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("UI World"),
                entries: &[
                    storage_entry(0, both),
                    storage_entry(1, wgpu::ShaderStages::VERTEX),
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            }),
            placements: placement_buffer(device, 8),
            capacity: 8,
            revision: None,
            centres: Vec::new(),
            group: None,
        });
        let mut layouts = vec![Some(&layout), Some(&textures_layout)];
        if let Some(world) = &world {
            layouts.push(Some(&world.layout));
        }
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("UI"),
            bind_group_layouts: &layouts,
            immediate_size: 0,
        });
        let pipeline = helio_core::shader::create_render_pipeline(
            device,
            wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            },
        );
        Self {
            kind,
            pipeline,
            layout,
            textures_layout,
            material_binding,
            atlas,
            globals: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("UI Globals"),
                size: std::mem::size_of::<Globals>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            shown_globals: None,
            linear_output: target.is_srgb(),
            primitives: primitive_buffer(device, FIRST_PRIMITIVES),
            capacity: FIRST_PRIMITIVES,
            heap: Heap::default(),
            group: None,
            textures: None,
            stand_in: helio_mats::MaterialStandIn::new(device, queue),
            world,
            order: Vec::new(),
            visible: false,
        }
    }

    fn prepare_layer(&mut self, ctx: &PrepareContext, frame: &UiFrame) {
        let layer = match self.kind {
            UiLayerKind::Screen => &frame.screen,
            UiLayerKind::World => &frame.world,
        };
        if self.heap.is_stale(layer) {
            // Room for the worst case: every batch rewritten into new runs.
            let required = Heap::required(layer).max(self.heap.len() as usize);
            if required > self.capacity {
                self.capacity = required.next_power_of_two();
                self.primitives = primitive_buffer(ctx.device, self.capacity);
                self.heap.invalidate();
                self.group = None;
            }
            let buffer = &self.primitives;
            self.heap.sync(layer, &mut |write| {
                ctx.write_buffer(
                    buffer,
                    u64::from(write.start) * PRIMITIVE_BYTES,
                    bytemuck::cast_slice(write.primitives),
                );
            });
        }
        if let Some(world) = &mut self.world {
            if world.revision != Some(frame.placements.revision) {
                world.revision = Some(frame.placements.revision);
                let rows = &frame.placements.rows;
                if rows.len() > world.capacity {
                    world.capacity = rows.len().next_power_of_two();
                    world.placements = placement_buffer(ctx.device, world.capacity);
                    world.group = None;
                }
                if !rows.is_empty() {
                    ctx.write_buffer(&world.placements, 0, bytemuck::cast_slice(rows));
                }
                world.centres.clear();
                world
                    .centres
                    .extend(rows.iter().map(|r| [r.centre[0], r.centre[1], r.centre[2]]));
            }
            // Back to front: translucent widgets blend over the ones behind.
            let eye = ctx.camera_data.position_near;
            self.order.clear();
            for (index, run) in self.heap.runs().iter().enumerate() {
                let centre = world
                    .centres
                    .get(run.placement as usize)
                    .copied()
                    .unwrap_or_default();
                let d: f32 = (0..3).map(|i| (centre[i] - eye[i]).powi(2)).sum();
                self.order.push((d, index as u32));
            }
            self.order
                .sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        }
    }
}

impl RenderPass for UiPass {
    fn name(&self) -> &'static str {
        match self.kind {
            UiLayerKind::Screen => helio_ui_api::SCREEN_PASS_NAME,
            UiLayerKind::World => helio_ui_api::WORLD_PASS_NAME,
        }
    }

    fn reads(&self) -> &'static [&'static str] {
        match self.kind {
            UiLayerKind::Screen => &[],
            UiLayerKind::World => &[helio_frame_api::gbuffer::DEPTH],
        }
    }

    fn declare_resources(&self, builder: &mut ResourceBuilder) {
        builder.read_optional(helio_mats::MATERIAL_TEXTURES);
        if self.kind == UiLayerKind::World {
            builder.read(helio_frame_api::gbuffer::DEPTH);
        }
    }

    fn prepare(&mut self, ctx: &PrepareContext) -> HelioResult<()> {
        let frame = ctx.scene_buffers.payload(&UI_FRAME);
        self.visible = false;
        let Some(frame) = frame else {
            return Ok(());
        };
        self.prepare_layer(ctx, &frame);
        self.visible = !self.heap.runs().is_empty();
        if self.visible {
            self.atlas.sync(ctx.queue, &frame);
        }
        Ok(())
    }

    fn render_pass_descriptor_with_storage<'a>(
        &'a self,
        target: &'a wgpu::TextureView,
        _depth: &'a wgpu::TextureView,
        _resources: &'a helio_core::ResourceRegistry<'a>,
        storage: &'a mut helio_core::RenderFrameStorage,
    ) -> Option<wgpu::RenderPassDescriptor<'a>> {
        let attachments: &'a [Option<wgpu::RenderPassColorAttachment<'a>>] = storage
            .retain_boxed_slice(Box::new([Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })]));
        Some(wgpu::RenderPassDescriptor {
            label: Some("UI Pass"),
            color_attachments: attachments,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    fn execute(&mut self, ctx: &mut PassContext) -> HelioResult<()> {
        if !self.visible {
            return Ok(());
        }
        let textures = ctx
            .registry
            .get::<helio_mats::MaterialTextureBindings<'_>>(helio_mats::material_textures())
            .unwrap_or_else(|| self.stand_in.textures());
        let size = ctx.target.texture().size();
        let globals = Globals {
            output: [size.width as f32, size.height as f32],
            linear_output: u32::from(self.linear_output),
            pad: 0,
        };
        if self.shown_globals != Some(globals) {
            self.shown_globals = Some(globals);
            helio_core::upload::write_buffer(ctx.queue, &self.globals, 0, bytemuck::bytes_of(&globals));
        }
        if self.textures.as_ref().map(|t| t.0) != Some(textures.version) {
            let mut entries = Vec::new();
            self.material_binding.append_bind_group_entries(
                &mut entries,
                0,
                textures.texture_views,
                textures.sampler,
            );
            let group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("UI Textures"),
                layout: &self.textures_layout,
                entries: &entries,
            });
            self.textures = Some((textures.version, group));
        }
        if self.group.is_none() {
            self.group = Some(ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("UI Layer"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.globals.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.primitives.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&self.atlas.0.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.atlas.0.sampler),
                    },
                ],
            }));
        }
        if let Some(world) = &mut self.world {
            let stale = world
                .group
                .as_ref()
                .is_none_or(|(depth, camera, _)| depth != ctx.depth || camera != ctx.camera);
            if stale {
                let group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("UI World"),
                    layout: &world.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: ctx.camera.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: world.placements.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(ctx.depth),
                        },
                    ],
                });
                world.group = Some((ctx.depth.clone(), ctx.camera.clone(), group));
            }
        }
        let count = self.heap.len();
        let Some(mut rp) = ctx.render_cmds() else {
            return Ok(());
        };
        rp.set_pipeline(&self.pipeline);
        let (Some(group), Some((_, textures))) = (&self.group, &self.textures) else {
            return Ok(());
        };
        rp.set_bind_group(0, group, &[]);
        rp.set_bind_group(1, textures, &[]);
        match &self.world {
            None => rp.draw(0..6, 0..count),
            Some(world) => {
                let Some((_, _, group)) = &world.group else {
                    return Ok(());
                };
                rp.set_bind_group(2, group, &[]);
                for (_, index) in &self.order {
                    let run = self.heap.runs()[*index as usize];
                    rp.draw(0..6, run.start..run.start + run.len);
                }
            }
        }
        Ok(())
    }
}
