//! GPU edge preparation and compact, classified indirect draws.
//! The records are generated entirely on GPU; there is no ribbon upload.

use vello::wgpu;

use super::super::ModelComposite;

/// 0..4 triangle DrawIndirectArgs; 4..8 parallel DrawIndirectArgs;
/// 8 input edges, 9 culled edges, 10 body triangles, 11 ordinary wing triangles,
/// 12 crossing triangles, 13 parallel edges, 14 culled triangles, 15 reserved.
/// 16..20 regular args, 20..24 masked-lobe args, 24..28 partial-body args.
pub(super) const STATS_WORDS: usize = 28;

pub(super) struct PreparedRibbons {
    prepare_pipeline: wgpu::ComputePipeline,
    prepare_bindings: wgpu::BindGroup,
    prefix_pipeline: wgpu::ComputePipeline,
    scatter_pipeline: wgpu::ComputePipeline,
    prefix_bindings: wgpu::BindGroup,
    render_bindings: wgpu::BindGroup,
    triangles_additive: wgpu::RenderPipeline,
    triangles_multiply: wgpu::RenderPipeline,
    lobes_additive: wgpu::RenderPipeline,
    lobes_multiply: wgpu::RenderPipeline,
    bodies_additive: wgpu::RenderPipeline,
    bodies_multiply: wgpu::RenderPipeline,
    parallel_additive: wgpu::RenderPipeline,
    parallel_multiply: wgpu::RenderPipeline,
    counters: wgpu::Buffer,
    edge_count: u32,
}

impl PreparedRibbons {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        uniforms: &wgpu::Buffer,
        edges: &wgpu::Buffer,
        history: &wgpu::Buffer,
        edge_count: u32,
    ) -> Self {
        let storage = |label, bytes| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            })
        };
        // A record is 64 bytes: positions, alphas, two prepared AA planes,
        // and a small mode mask. This omits repeated RGB and temporal geometry.
        let triangles = storage(
            "PROTOTYPE GPU compact triangles",
            u64::from(edge_count.max(1)) * 6 * 64,
        );
        let parallel = storage(
            "PROTOTYPE GPU parallel intervals",
            u64::from(edge_count.max(1)) * 2 * 64,
        );
        let triangle_indices = storage(
            "PROTOTYPE classified compact triangle indices",
            u64::from(edge_count.max(1)) * (6 * 3 + 2) * 4,
        );
        let edge_masks = storage(
            "PROTOTYPE deterministic edge membership",
            u64::from(edge_count.max(1)) * 16,
        );
        let edge_offsets = storage(
            "PROTOTYPE deterministic local edge offsets",
            u64::from(edge_count.max(1)) * 16,
        );
        let block_totals = storage(
            "PROTOTYPE deterministic block totals",
            u64::from(edge_count.max(1).div_ceil(64)) * 16,
        );
        let body_data = storage(
            "PROTOTYPE shared body attachment coverage",
            u64::from(edge_count.max(1)) * 32,
        );
        let counters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PROTOTYPE indirect arguments and edge statistics"),
            size: (STATS_WORDS * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        // Reuse the direct path's pure geometry helpers, so the optimization
        // changes execution and payload rather than maintaining a second model.
        let direct = include_str!("ribbons.wgsl");
        let start = direct
            .find("// FAST GEOMETRY:")
            .expect("shared fast geometry marker");
        let end = direct
            .find("@vertex\nfn ribbon_fast_vertex")
            .expect("fast entry point marker");
        let source =
            include_str!("prepared.wgsl").replace("// SHARED_FAST_GEOMETRY", &direct[start..end]);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PROTOTYPE prepare classified ribbons"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let entry = |binding, visibility, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let prepare_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PROTOTYPE edge preparation bindings"),
            entries: &[
                entry(
                    0,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Uniform,
                ),
                entry(
                    1,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                entry(
                    2,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                entry(
                    3,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
                entry(
                    4,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
                entry(
                    5,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
                entry(
                    6,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
                entry(
                    7,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
            ],
        });
        fn bind(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        let prepare_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PROTOTYPE edge preparation resources"),
            layout: &prepare_layout,
            entries: &[
                bind(0, uniforms),
                bind(1, edges),
                bind(2, history),
                bind(3, &triangles),
                bind(4, &parallel),
                bind(5, &counters),
                bind(6, &body_data),
                bind(7, &edge_masks),
            ],
        });
        let prepare_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("PROTOTYPE prepare each edge once"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("PROTOTYPE edge preparation layout"),
                    bind_group_layouts: &[Some(&prepare_layout)],
                    immediate_size: 0,
                }),
            ),
            module: &shader,
            entry_point: Some("prepare_edges"),
            compilation_options: Default::default(),
            cache: None,
        });
        let render_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PROTOTYPE classified ribbon draw bindings"),
            entries: &[
                entry(
                    0,
                    wgpu::ShaderStages::VERTEX_FRAGMENT,
                    wgpu::BufferBindingType::Uniform,
                ),
                entry(
                    1,
                    wgpu::ShaderStages::VERTEX,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                entry(
                    2,
                    wgpu::ShaderStages::VERTEX,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                entry(
                    3,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                entry(
                    4,
                    wgpu::ShaderStages::VERTEX,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
            ],
        });
        let render_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PROTOTYPE classified ribbon draw resources"),
            layout: &render_layout,
            entries: &[
                bind(0, uniforms),
                bind(1, &triangles),
                bind(2, &parallel),
                bind(3, &body_data),
                bind(4, &triangle_indices),
            ],
        });
        let empty = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PROTOTYPE unused preparation group while rendering"),
            entries: &[],
        });
        let prefix_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PROTOTYPE deterministic list compaction bindings"),
            entries: &[
                entry(
                    0,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                entry(
                    1,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
                entry(
                    2,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
                entry(
                    3,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
            ],
        });
        let prefix_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PROTOTYPE deterministic list compaction resources"),
            layout: &prefix_layout,
            entries: &[
                bind(0, &edge_masks),
                bind(1, &edge_offsets),
                bind(2, &block_totals),
                bind(3, &triangle_indices),
            ],
        });
        let prefix_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("PROTOTYPE deterministic list compaction layout"),
                bind_group_layouts: &[Some(&empty), Some(&empty), Some(&prefix_layout)],
                immediate_size: 0,
            });
        let prefix_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("PROTOTYPE scan local edge membership"),
            layout: Some(&prefix_pipeline_layout),
            module: &shader,
            entry_point: Some("prefix_edges"),
            compilation_options: Default::default(),
            cache: None,
        });
        let scatter_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("PROTOTYPE stable classified index scatter"),
            layout: Some(&prefix_pipeline_layout),
            module: &shader,
            entry_point: Some("scatter_edges"),
            compilation_options: Default::default(),
            cache: None,
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PROTOTYPE classified ribbon pipeline layout"),
            bind_group_layouts: &[Some(&empty), Some(&render_layout)],
            immediate_size: 0,
        });
        let pipeline = |label, vertex_entry, fragment_entry, multiply| {
            let component = if multiply {
                wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Dst,
                    dst_factor: wgpu::BlendFactor::Zero,
                    operation: wgpu::BlendOperation::Add,
                }
            } else {
                wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                }
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vertex_entry),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment_entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState {
                            color: component,
                            alpha: wgpu::BlendComponent::OVER,
                        }),
                        write_mask: wgpu::ColorWrites::COLOR,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let triangles_additive = pipeline(
            "PROTOTYPE compact triangles additive",
            "regular_vertex",
            "regular_additive",
            false,
        );
        let triangles_multiply = pipeline(
            "PROTOTYPE compact triangles multiply",
            "regular_vertex",
            "regular_multiply",
            true,
        );
        let lobes_additive = pipeline(
            "PROTOTYPE masked lobes additive",
            "lobe_vertex",
            "lobe_additive",
            false,
        );
        let lobes_multiply = pipeline(
            "PROTOTYPE masked lobes multiply",
            "lobe_vertex",
            "lobe_multiply",
            true,
        );
        let bodies_additive = pipeline(
            "PROTOTYPE partial body additive",
            "body_vertex",
            "body_additive",
            false,
        );
        let bodies_multiply = pipeline(
            "PROTOTYPE partial body multiply",
            "body_vertex",
            "body_multiply",
            true,
        );
        let parallel_additive = pipeline(
            "PROTOTYPE parallel intervals additive",
            "parallel_vertex",
            "parallel_additive",
            false,
        );
        let parallel_multiply = pipeline(
            "PROTOTYPE parallel intervals multiply",
            "parallel_vertex",
            "parallel_multiply",
            true,
        );
        Self {
            prepare_pipeline,
            prepare_bindings,
            prefix_pipeline,
            scatter_pipeline,
            prefix_bindings,
            render_bindings,
            triangles_additive,
            triangles_multiply,
            lobes_additive,
            lobes_multiply,
            bodies_additive,
            bodies_multiply,
            parallel_additive,
            parallel_multiply,
            counters,
            edge_count,
        }
    }

    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        // Reset only GPU-owned counters; no host staging write is needed.
        encoder.clear_buffer(&self.counters, 0, None);
        let groups = self.edge_count.max(1).div_ceil(64);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PROTOTYPE prepare active edge triangles"),
                timestamp_writes: timestamps.as_ref().map(|value| {
                    wgpu::ComputePassTimestampWrites {
                        query_set: value.query_set,
                        beginning_of_pass_write_index: value.beginning_of_pass_write_index,
                        end_of_pass_write_index: None,
                    }
                }),
            });
            pass.set_pipeline(&self.prepare_pipeline);
            pass.set_bind_group(0, &self.prepare_bindings, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PROTOTYPE scan local edge membership"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.prefix_pipeline);
            pass.set_bind_group(2, &self.prefix_bindings, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PROTOTYPE stable classified index scatter"),
                timestamp_writes: timestamps.as_ref().map(|value| {
                    wgpu::ComputePassTimestampWrites {
                        query_set: value.query_set,
                        beginning_of_pass_write_index: None,
                        end_of_pass_write_index: value.end_of_pass_write_index,
                    }
                }),
            });
            pass.set_pipeline(&self.scatter_pipeline);
            pass.set_bind_group(2, &self.prefix_bindings, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, composite: ModelComposite) {
        pass.set_bind_group(1, &self.render_bindings, &[]);
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.triangles_additive,
            ModelComposite::Multiplicative => &self.triangles_multiply,
        });
        pass.draw_indirect(&self.counters, 64);
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.lobes_additive,
            ModelComposite::Multiplicative => &self.lobes_multiply,
        });
        pass.draw_indirect(&self.counters, 80);
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.bodies_additive,
            ModelComposite::Multiplicative => &self.bodies_multiply,
        });
        pass.draw_indirect(&self.counters, 96);
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.parallel_additive,
            ModelComposite::Multiplicative => &self.parallel_multiply,
        });
        pass.draw_indirect(&self.counters, 16);
    }

    pub fn stats_buffer(&self) -> &wgpu::Buffer {
        &self.counters
    }
}
