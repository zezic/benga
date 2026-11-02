//! Production camera/part projection. The three screen-space poses stay on GPU.
use vello::wgpu;

use super::super::gpu_bridge::GpuProjection;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProjectionParams {
    projection: GpuProjection,
    // Vertex count, newest ring index, history reseed flag, reserved.
    history: [u32; 4],
}

pub(super) struct ProjectionPipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

pub(super) struct ProjectedPose {
    uniforms: wgpu::Buffer,
    bindings: wgpu::BindGroup,
}

impl ProjectionPipeline {
    pub fn new(device: &wgpu::Device) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Production GPU camera projection bindings"),
            entries: &[
                entry(0, wgpu::BufferBindingType::Uniform),
                entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Production GPU camera and slide projection"),
            source: wgpu::ShaderSource::Wgsl(include_str!("projection.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Production shared vertex projection"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("Production GPU camera projection layout"),
                    bind_group_layouts: &[Some(&layout)],
                    immediate_size: 0,
                }),
            ),
            module: &shader,
            entry_point: Some("project_vertices"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline, layout }
    }

    pub fn bind(
        &self,
        device: &wgpu::Device,
        source: &wgpu::Buffer,
        history: &wgpu::Buffer,
    ) -> ProjectedPose {
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Production GPU camera and slide parameters"),
            size: std::mem::size_of::<ProjectionParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Production shared vertex projection"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: source.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: history.as_entire_binding(),
                },
            ],
        });
        ProjectedPose { uniforms, bindings }
    }

    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, pose: &ProjectedPose, vertices: u32) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Production GPU camera projection"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &pose.bindings, &[]);
        pass.dispatch_workgroups(vertices.div_ceil(64), 1, 1);
    }
}

impl ProjectedPose {
    pub fn update(
        &self,
        queue: &wgpu::Queue,
        projection: &GpuProjection,
        vertices: u32,
        newest: u32,
        reseed: bool,
    ) {
        queue.write_buffer(
            &self.uniforms,
            0,
            bytemuck::bytes_of(&ProjectionParams {
                projection: *projection,
                history: [vertices, newest, u32::from(reseed), 0],
            }),
        );
    }
}
