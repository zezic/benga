//! GPU edge preparation and compact, classified indirect draws.
//! The records are generated entirely on GPU; there is no ribbon upload.

use std::sync::Arc;

use vello::wgpu;

use super::super::ModelComposite;

/// 0..4 triangle DrawIndirectArgs; 4..8 parallel DrawIndirectArgs;
/// 8 input edges, 9 culled edges, 10 body triangles, 11 ordinary wing triangles,
/// 12 crossing triangles, 13 parallel edges, 14 culled triangles, 15 reserved.
/// 16..20 regular args, 20..24 masked-lobe args, 24..28 partial-body args.
pub(super) const STATS_WORDS: usize = 28;

pub(super) struct RibbonPipelines {
    #[cfg(test)]
    tight_parallel_support: bool,
    prepare_pipeline: wgpu::ComputePipeline,
    prefix_pipeline: wgpu::ComputePipeline,
    scatter_pipeline: wgpu::ComputePipeline,
    triangles_additive: wgpu::RenderPipeline,
    triangles_multiply: wgpu::RenderPipeline,
    lobes_additive: wgpu::RenderPipeline,
    lobes_multiply: wgpu::RenderPipeline,
    bodies_additive: wgpu::RenderPipeline,
    bodies_multiply: wgpu::RenderPipeline,
    parallel_additive: wgpu::RenderPipeline,
    parallel_multiply: wgpu::RenderPipeline,
    prepare_layout: wgpu::BindGroupLayout,
    render_layout: wgpu::BindGroupLayout,
    prefix_layout: wgpu::BindGroupLayout,
}

pub(super) struct PreparedRibbons {
    pipelines: Arc<RibbonPipelines>,
    prepare_bindings: wgpu::BindGroup,
    prefix_bindings: wgpu::BindGroup,
    render_bindings: wgpu::BindGroup,
    counters: wgpu::Buffer,
    edge_count: u32,
    storage: RibbonStorage,
}

/// Large working buffers survive model exchanges. Bind groups expose only the
/// current model's range so WGSL array lengths keep their original meaning.
struct RibbonStorage {
    triangles: wgpu::Buffer,
    parallel: wgpu::Buffer,
    triangle_indices: wgpu::Buffer,
    edge_masks: wgpu::Buffer,
    edge_offsets: wgpu::Buffer,
    block_totals: wgpu::Buffer,
    body_data: wgpu::Buffer,
}

#[cfg(test)]
pub(super) struct BenchmarkBuffers<'a> {
    pub triangles: &'a wgpu::Buffer,
    pub parallel: &'a wgpu::Buffer,
    pub indices: &'a wgpu::Buffer,
    pub counters: &'a wgpu::Buffer,
    pub edge_count: u32,
    pub tight_parallel_support: bool,
}

impl RibbonPipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        // Diagnostic comparison only: retain the original support by default
        // until full-scene measurements and image comparisons justify a change.
        Self::with_parallel_support(
            device,
            std::env::var("BENGA_GPU_RIBBON_PARALLEL_BOUNDS").as_deref() == Ok("tight"),
        )
    }

    pub(super) fn with_parallel_support(device: &wgpu::Device, tight: bool) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;
        // Frozen prototype geometry with production per-edge ink and timestamps.
        let source = include_str!("prepared.wgsl")
            .replace("// SHARED_FAST_GEOMETRY", include_str!("geometry.wgsl"))
            .replace(
                "// PARALLEL_SUPPORT_MODE",
                if tight {
                    "const TIGHT_PARALLEL_SUPPORT: bool = true;"
                } else {
                    "const TIGHT_PARALLEL_SUPPORT: bool = false;"
                },
            );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Experimental prepare classified ribbons"),
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
            label: Some("Experimental edge preparation bindings"),
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
                entry(
                    8,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
            ],
        });
        let prepare_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Experimental prepare each edge once"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("Experimental edge preparation layout"),
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
            label: Some("Experimental classified ribbon draw bindings"),
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
                entry(
                    5,
                    wgpu::ShaderStages::VERTEX,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
            ],
        });
        let empty = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Experimental unused preparation group while rendering"),
            entries: &[],
        });
        let prefix_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Experimental deterministic list compaction bindings"),
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
        let prefix_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Experimental deterministic list compaction layout"),
                bind_group_layouts: &[Some(&empty), Some(&empty), Some(&prefix_layout)],
                immediate_size: 0,
            });
        let prefix_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Experimental scan local edge membership"),
            layout: Some(&prefix_pipeline_layout),
            module: &shader,
            entry_point: Some("prefix_edges"),
            compilation_options: Default::default(),
            cache: None,
        });
        let scatter_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Experimental stable classified index scatter"),
            layout: Some(&prefix_pipeline_layout),
            module: &shader,
            entry_point: Some("scatter_edges"),
            compilation_options: Default::default(),
            cache: None,
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Experimental classified ribbon pipeline layout"),
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
            "Experimental compact triangles additive",
            "regular_vertex",
            "regular_additive",
            false,
        );
        let triangles_multiply = pipeline(
            "Experimental compact triangles multiply",
            "regular_vertex",
            "regular_multiply",
            true,
        );
        let lobes_additive = pipeline(
            "Experimental masked lobes additive",
            "lobe_vertex",
            "lobe_additive",
            false,
        );
        let lobes_multiply = pipeline(
            "Experimental masked lobes multiply",
            "lobe_vertex",
            "lobe_multiply",
            true,
        );
        let bodies_additive = pipeline(
            "Experimental partial body additive",
            "body_vertex",
            "body_additive",
            false,
        );
        let bodies_multiply = pipeline(
            "Experimental partial body multiply",
            "body_vertex",
            "body_multiply",
            true,
        );
        let parallel_additive = pipeline(
            "Experimental parallel intervals additive",
            "parallel_vertex",
            "parallel_additive",
            false,
        );
        let parallel_multiply = pipeline(
            "Experimental parallel intervals multiply",
            "parallel_vertex",
            "parallel_multiply",
            true,
        );
        Self {
            #[cfg(test)]
            tight_parallel_support: tight,
            prepare_pipeline,
            prefix_pipeline,
            scatter_pipeline,
            triangles_additive,
            triangles_multiply,
            lobes_additive,
            lobes_multiply,
            bodies_additive,
            bodies_multiply,
            parallel_additive,
            parallel_multiply,
            prepare_layout,
            render_layout,
            prefix_layout,
        }
    }
}

impl PreparedRibbons {
    pub fn new(
        device: &wgpu::Device,
        pipelines: Arc<RibbonPipelines>,
        uniforms: &wgpu::Buffer,
        edges: &wgpu::Buffer,
        history: &wgpu::Buffer,
        styles: &wgpu::Buffer,
        edge_count: u32,
    ) -> Self {
        let storage = |label: &str, bytes: u64, extra_usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | extra_usage,
                mapped_at_creation: false,
            })
        };
        #[cfg(test)]
        let diagnostic_usage = wgpu::BufferUsages::COPY_SRC;
        #[cfg(not(test))]
        let diagnostic_usage = wgpu::BufferUsages::empty();
        // A record is 64 bytes: positions, alphas, two prepared AA planes,
        // and a small mode mask. This omits repeated RGB and temporal geometry.
        let triangles = storage(
            "Experimental GPU compact triangles",
            u64::from(edge_count.max(1)) * 6 * 64,
            diagnostic_usage,
        );
        let parallel = storage(
            "Experimental GPU parallel intervals",
            u64::from(edge_count.max(1)) * 2 * 64,
            diagnostic_usage,
        );
        let triangle_indices = storage(
            "Experimental classified compact triangle indices",
            u64::from(edge_count.max(1)) * (6 * 3 + 2) * 4,
            diagnostic_usage,
        );
        let edge_masks = storage(
            "Experimental deterministic edge membership",
            u64::from(edge_count.max(1)) * 16,
            wgpu::BufferUsages::empty(),
        );
        let edge_offsets = storage(
            "Experimental deterministic local edge offsets",
            u64::from(edge_count.max(1)) * 16,
            wgpu::BufferUsages::empty(),
        );
        let block_totals = storage(
            "Experimental deterministic block totals",
            u64::from(edge_count.max(1).div_ceil(64)) * 16,
            wgpu::BufferUsages::empty(),
        );
        let body_data = storage(
            "Experimental shared body attachment coverage",
            u64::from(edge_count.max(1)) * 32,
            wgpu::BufferUsages::empty(),
        );
        let counters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Experimental indirect arguments and edge statistics"),
            size: (STATS_WORDS * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_DST
                | diagnostic_usage,
            mapped_at_creation: false,
        });
        let storage = RibbonStorage {
            triangles,
            parallel,
            triangle_indices,
            edge_masks,
            edge_offsets,
            block_totals,
            body_data,
        };
        let (prepare_bindings, render_bindings, prefix_bindings) = Self::bindings(
            device, &pipelines, uniforms, edges, history, styles, &counters, &storage, edge_count,
        );
        Self {
            pipelines,
            prepare_bindings,
            prefix_bindings,
            render_bindings,
            counters,
            edge_count,
            storage,
        }
    }

    fn bindings(
        device: &wgpu::Device,
        pipelines: &RibbonPipelines,
        uniforms: &wgpu::Buffer,
        edges: &wgpu::Buffer,
        history: &wgpu::Buffer,
        styles: &wgpu::Buffer,
        counters: &wgpu::Buffer,
        storage: &RibbonStorage,
        edge_count: u32,
    ) -> (wgpu::BindGroup, wgpu::BindGroup, wgpu::BindGroup) {
        fn bind(binding: u32, buffer: &wgpu::Buffer, bytes: u64) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(bytes),
                }),
            }
        }
        let count = u64::from(edge_count.max(1));
        let prepare_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Experimental edge preparation resources"),
            layout: &pipelines.prepare_layout,
            entries: &[
                bind(0, uniforms, uniforms.size()),
                bind(1, edges, count * 8),
                bind(2, history, history.size()),
                bind(3, &storage.triangles, count * 6 * 64),
                bind(4, &storage.parallel, count * 2 * 64),
                bind(5, counters, counters.size()),
                bind(6, &storage.body_data, count * 32),
                bind(7, &storage.edge_masks, count * 16),
                bind(8, styles, count * 16),
            ],
        });
        let render_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Experimental classified ribbon draw resources"),
            layout: &pipelines.render_layout,
            entries: &[
                bind(0, uniforms, uniforms.size()),
                bind(1, &storage.triangles, count * 6 * 64),
                bind(2, &storage.parallel, count * 2 * 64),
                bind(3, &storage.body_data, count * 32),
                bind(4, &storage.triangle_indices, count * (6 * 3 + 2) * 4),
                bind(5, styles, count * 16),
            ],
        });
        let prefix_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Experimental deterministic list compaction resources"),
            layout: &pipelines.prefix_layout,
            entries: &[
                bind(0, &storage.edge_masks, count * 16),
                bind(1, &storage.edge_offsets, count * 16),
                bind(2, &storage.block_totals, count.div_ceil(64) * 16),
                bind(3, &storage.triangle_indices, count * (6 * 3 + 2) * 4),
            ],
        });
        (prepare_bindings, render_bindings, prefix_bindings)
    }

    /// Reuse high-water scratch storage while exposing the new model's actual
    /// edge count to compaction and indirect-draw indexing.
    pub fn rebind(
        &mut self,
        device: &wgpu::Device,
        uniforms: &wgpu::Buffer,
        edges: &wgpu::Buffer,
        history: &wgpu::Buffer,
        styles: &wgpu::Buffer,
        edge_count: u32,
    ) {
        let (prepare, render, prefix) = Self::bindings(
            device,
            &self.pipelines,
            uniforms,
            edges,
            history,
            styles,
            &self.counters,
            &self.storage,
            edge_count,
        );
        self.prepare_bindings = prepare;
        self.render_bindings = render;
        self.prefix_bindings = prefix;
        self.edge_count = edge_count;
    }

    #[cfg(test)]
    pub(super) fn benchmark_buffers(&self) -> BenchmarkBuffers<'_> {
        BenchmarkBuffers {
            triangles: &self.storage.triangles,
            parallel: &self.storage.parallel,
            indices: &self.storage.triangle_indices,
            counters: &self.counters,
            edge_count: self.edge_count,
            tight_parallel_support: self.pipelines.tight_parallel_support,
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
                label: Some("Experimental prepare active edge triangles"),
                timestamp_writes: timestamps.as_ref().map(|value| {
                    wgpu::ComputePassTimestampWrites {
                        query_set: value.query_set,
                        beginning_of_pass_write_index: value.beginning_of_pass_write_index,
                        end_of_pass_write_index: None,
                    }
                }),
            });
            pass.set_pipeline(&self.pipelines.prepare_pipeline);
            pass.set_bind_group(0, &self.prepare_bindings, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Experimental scan local edge membership"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.prefix_pipeline);
            pass.set_bind_group(2, &self.prefix_bindings, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Experimental stable classified index scatter"),
                timestamp_writes: timestamps.as_ref().map(|value| {
                    wgpu::ComputePassTimestampWrites {
                        query_set: value.query_set,
                        beginning_of_pass_write_index: None,
                        end_of_pass_write_index: value.end_of_pass_write_index,
                    }
                }),
            });
            pass.set_pipeline(&self.pipelines.scatter_pipeline);
            pass.set_bind_group(2, &self.prefix_bindings, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, composite: ModelComposite) {
        pass.set_bind_group(1, &self.render_bindings, &[]);
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.pipelines.triangles_additive,
            ModelComposite::Multiplicative => &self.pipelines.triangles_multiply,
        });
        pass.draw_indirect(&self.counters, 64);
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.pipelines.lobes_additive,
            ModelComposite::Multiplicative => &self.pipelines.lobes_multiply,
        });
        pass.draw_indirect(&self.counters, 80);
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.pipelines.bodies_additive,
            ModelComposite::Multiplicative => &self.pipelines.bodies_multiply,
        });
        pass.draw_indirect(&self.counters, 96);
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.pipelines.parallel_additive,
            ModelComposite::Multiplicative => &self.pipelines.parallel_multiply,
        });
        pass.draw_indirect(&self.counters, 16);
    }
}
