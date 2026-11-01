//! Throwaway GPU experiment: static topology, projected shared-vertex history,
//! and procedural interval ribbons. This is deliberately separate from playback.
use super::prepared::PreparedRibbons;
use vello::wgpu::{self, util::DeviceExt};

use super::super::{
    geometry::{Camera, Pose, Vec3},
    LineWidth, ModelComposite,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct FrameParams {
    pub rotation: [[f32; 4]; 3],
    pub viewport: [f32; 4],
    pub camera: [f32; 4],
    pub pose: [f32; 4],
    /// Stroke width, base alpha, motion-fade power, motion-opacity floor.
    pub style: [f32; 4],
    pub tint: [f32; 4],
    /// Vertex count, newest ring slot, oldest ring slot, reserved.
    pub history: [u32; 4],
}

impl FrameParams {
    pub fn new(width: u32, height: u32, pose: Pose, slot: u32, alpha: f32) -> Self {
        let camera = Camera::new(width as f32, height as f32, 1.0);
        let column = |v: Vec3| {
            let p = pose.orient(v);
            [p.x, p.y, p.z, 0.0]
        };
        Self {
            rotation: [
                column(Vec3::new(1.0, 0.0, 0.0)),
                column(Vec3::new(0.0, 1.0, 0.0)),
                column(Vec3::new(0.0, 0.0, 1.0)),
            ],
            viewport: [width as f32, height as f32, 0.0, 0.0],
            camera: [camera.center.x, camera.center.y, camera.focal, 0.4],
            pose: [pose.distance, pose.shake.x, pose.shake.y, pose.scale],
            style: [
                (width.min(height) as f32 / 440.0).clamp(0.62, 2.6)
                    * 1.4
                    * LineWidth::default().factor(),
                alpha,
                0.5,
                0.0,
            ],
            tint: [0.3, 0.8, 1.0, 1.0],
            history: [0, slot % 3, (slot + 1) % 3, 0],
        }
    }
}

pub(super) struct GpuRibbons {
    uniforms: wgpu::Buffer,
    project_pipeline: wgpu::ComputePipeline,
    project_bindings: wgpu::BindGroup,
    additive_pipeline: wgpu::RenderPipeline,
    multiply_pipeline: wgpu::RenderPipeline,
    indexed_additive_pipeline: wgpu::RenderPipeline,
    indexed_multiply_pipeline: wgpu::RenderPipeline,
    coverage_indices: wgpu::Buffer,
    fast_additive_pipeline: wgpu::RenderPipeline,
    fast_multiply_pipeline: wgpu::RenderPipeline,
    baseline_additive_pipeline: wgpu::RenderPipeline,
    baseline_multiply_pipeline: wgpu::RenderPipeline,
    render_bindings: wgpu::BindGroup,
    pub projected_history: wgpu::Buffer,
    vertex_count: u32,
    edge_count: u32,
    prepared: PreparedRibbons,
}

impl GpuRibbons {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        vertices: &[[f32; 4]],
        edges: &[[u32; 2]],
    ) -> Self {
        let vertex_count = vertices.len() as u32;
        let edge_count = edges.len() as u32;
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("PROTOTYPE static model vertices"),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let edges = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("PROTOTYPE static model edges"),
            contents: bytemuck::cast_slice(edges),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let coverage_indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("PROTOTYPE static coverage quad indices"),
            contents: bytemuck::cast_slice(&[0_u16, 1, 2, 2, 1, 3]),
            usage: wgpu::BufferUsages::INDEX,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PROTOTYPE frame parameters"),
            size: std::mem::size_of::<FrameParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let projected_history = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PROTOTYPE three projected-vertex history slots"),
            size: u64::from(vertex_count.max(1)) * 16 * 3,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PROTOTYPE GPU projected ribbons"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}",
                    include_str!("support_hull.wgsl"),
                    include_str!("ribbons.wgsl")
                )
                .into(),
            ),
        });
        // Frozen first prototype for same-process, same-format comparisons.
        let baseline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PROTOTYPE original GPU ribbon baseline"),
            source: wgpu::ShaderSource::Wgsl(include_str!("baseline.wgsl").into()),
        });
        let buffer_entry = |binding, visibility, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let project_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PROTOTYPE shared-vertex projection layout"),
            entries: &[
                buffer_entry(
                    0,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Uniform,
                ),
                buffer_entry(
                    1,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                buffer_entry(
                    2,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
            ],
        });
        let project_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PROTOTYPE shared-vertex projection bindings"),
            layout: &project_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: vertices.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: projected_history.as_entire_binding(),
                },
            ],
        });
        let project_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("PROTOTYPE project shared vertices"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("PROTOTYPE projection pipeline layout"),
                    bind_group_layouts: &[Some(&project_layout)],
                    immediate_size: 0,
                }),
            ),
            module: &shader,
            entry_point: Some("project_vertices"),
            compilation_options: Default::default(),
            cache: None,
        });
        // A different resource group keeps compute's writable ring out of the
        // render pipeline, which reads the same ring without a round trip.
        let render_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PROTOTYPE procedural ribbon layout"),
            entries: &[
                buffer_entry(
                    0,
                    wgpu::ShaderStages::VERTEX_FRAGMENT,
                    wgpu::BufferBindingType::Uniform,
                ),
                buffer_entry(
                    1,
                    wgpu::ShaderStages::VERTEX,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                buffer_entry(
                    2,
                    wgpu::ShaderStages::VERTEX,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
            ],
        });
        let render_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PROTOTYPE procedural ribbon bindings"),
            layout: &render_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: edges.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: projected_history.as_entire_binding(),
                },
            ],
        });
        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PROTOTYPE unused compute group during rendering"),
            entries: &[],
        });
        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("PROTOTYPE ribbon pipeline layout"),
                bind_group_layouts: &[Some(&empty_layout), Some(&render_layout)],
                immediate_size: 0,
            });
        let pipeline = |module: &wgpu::ShaderModule, label, vertex_entry, entry, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&render_pipeline_layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some(vertex_entry),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
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
        let additive_pipeline = pipeline(
            &shader,
            "PROTOTYPE additive ribbons",
            "ribbon_vertex",
            "ribbon_additive",
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            },
        );
        let multiply_pipeline = pipeline(
            &shader,
            "PROTOTYPE multiplicative ribbons",
            "ribbon_vertex",
            "ribbon_multiply",
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Dst,
                    dst_factor: wgpu::BlendFactor::Zero,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            },
        );
        let fast_additive_pipeline = pipeline(
            &shader,
            "PROTOTYPE vertex-colored additive ribbons",
            "ribbon_fast_vertex",
            "ribbon_fast_additive",
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            },
        );
        let fast_multiply_pipeline = pipeline(
            &shader,
            "PROTOTYPE vertex-colored multiplicative ribbons",
            "ribbon_fast_vertex",
            "ribbon_fast_multiply",
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Dst,
                    dst_factor: wgpu::BlendFactor::Zero,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            },
        );
        let additive = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        };
        let multiply = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Dst,
                dst_factor: wgpu::BlendFactor::Zero,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        };
        let baseline_additive_pipeline = pipeline(
            &baseline_shader,
            "PROTOTYPE original GPU additive",
            "ribbon_fast_vertex",
            "ribbon_fast_additive",
            additive,
        );
        let baseline_multiply_pipeline = pipeline(
            &baseline_shader,
            "PROTOTYPE original GPU multiply",
            "ribbon_fast_vertex",
            "ribbon_fast_multiply",
            multiply,
        );
        let indexed_additive_pipeline = pipeline(
            &shader,
            "PROTOTYPE indexed coverage additive",
            "ribbon_indexed_vertex",
            "ribbon_additive",
            additive,
        );
        let indexed_multiply_pipeline = pipeline(
            &shader,
            "PROTOTYPE indexed coverage multiply",
            "ribbon_indexed_vertex",
            "ribbon_multiply",
            multiply,
        );
        let prepared = PreparedRibbons::new(
            device,
            format,
            &uniforms,
            &edges,
            &projected_history,
            edge_count,
        );
        Self {
            uniforms,
            project_pipeline,
            project_bindings,
            additive_pipeline,
            multiply_pipeline,
            indexed_additive_pipeline,
            indexed_multiply_pipeline,
            coverage_indices,
            fast_additive_pipeline,
            fast_multiply_pipeline,
            baseline_additive_pipeline,
            baseline_multiply_pipeline,
            render_bindings,
            projected_history,
            vertex_count,
            edge_count,
            prepared,
        }
    }

    pub fn update(&self, queue: &wgpu::Queue, mut params: FrameParams) {
        params.history[0] = self.vertex_count;
        params.history[1] %= 3;
        params.history[2] = (params.history[1] + 1) % 3;
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&params));
    }

    pub fn encode_project(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("PROTOTYPE project into history ring"),
            timestamp_writes: timestamps,
        });
        pass.set_pipeline(&self.project_pipeline);
        pass.set_bind_group(0, &self.project_bindings, &[]);
        pass.dispatch_workgroups(self.vertex_count.div_ceil(64), 1, 1);
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, composite: ModelComposite) {
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.additive_pipeline,
            ModelComposite::Multiplicative => &self.multiply_pipeline,
        });
        pass.set_bind_group(1, &self.render_bindings, &[]);
        pass.draw(0..6, 0..self.edge_count * 2);
    }

    pub fn draw_indexed_coverage(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        composite: ModelComposite,
    ) {
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.indexed_additive_pipeline,
            ModelComposite::Multiplicative => &self.indexed_multiply_pipeline,
        });
        pass.set_bind_group(1, &self.render_bindings, &[]);
        pass.set_index_buffer(self.coverage_indices.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..6, 0, 0..self.edge_count * 2);
    }

    pub fn draw_fast(&self, pass: &mut wgpu::RenderPass<'_>, composite: ModelComposite) {
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.fast_additive_pipeline,
            ModelComposite::Multiplicative => &self.fast_multiply_pipeline,
        });
        pass.set_bind_group(1, &self.render_bindings, &[]);
        // One middle body plus two wings. Folded edges use two coverage quads
        // in the first twelve corners, with the last six corners degenerate.
        pass.draw(0..18, 0..self.edge_count);
    }

    pub fn draw_original(&self, pass: &mut wgpu::RenderPass<'_>, composite: ModelComposite) {
        pass.set_pipeline(match composite {
            ModelComposite::Additive => &self.baseline_additive_pipeline,
            ModelComposite::Multiplicative => &self.baseline_multiply_pipeline,
        });
        pass.set_bind_group(1, &self.render_bindings, &[]);
        pass.draw(0..18, 0..self.edge_count);
    }

    pub fn encode_prepare(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        self.prepared.encode(encoder, timestamps);
    }

    pub fn draw_prepared(&self, pass: &mut wgpu::RenderPass<'_>, composite: ModelComposite) {
        self.prepared.draw(pass, composite);
    }

    pub fn prepared_stats(&self) -> &wgpu::Buffer {
        self.prepared.stats_buffer()
    }
}
