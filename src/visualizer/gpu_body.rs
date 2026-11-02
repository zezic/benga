//! A retained coarse surface used only to give the model a solid backdrop.
//!
//! Triangle coverage is unioned before applying normal alpha once. A fading
//! body therefore has the same opacity at its front, back and intersecting parts.
//! The preceding displayed pose centers the body within the wireframe exposure.
use std::sync::Arc;

use vello::wgpu::{self, util::DeviceExt};

use super::gpu_bridge::{GpuBodyTopology, GpuModelFrame, GpuProjection};

mod history;
use history::BodyProjectionHistory;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BodyParams {
    projection: GpuProjection,
    /// Physical viewport dimensions, surface opacity, padding.
    viewport: [f32; 4],
}

struct StaticBody {
    source: Arc<GpuBodyTopology>,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
}

struct BodySlot {
    body: Option<Arc<StaticBody>>,
    uniforms: wgpu::Buffer,
    bindings: wgpu::BindGroup,
}

struct CoverageMask {
    view: wgpu::TextureView,
    bindings: wgpu::BindGroup,
    size: [u32; 2],
}

pub(super) struct GpuBodyRenderer {
    geometry_layout: wgpu::BindGroupLayout,
    mask_layout: wgpu::BindGroupLayout,
    coverage_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    topology: Vec<Arc<StaticBody>>,
    slots: Vec<BodySlot>,
    history: BodyProjectionHistory,
    mask: Option<CoverageMask>,
}

impl GpuBodyRenderer {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        background_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let geometry_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Model backdrop projection"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<BodyParams>() as u64
                    ),
                },
                count: None,
            }],
        });
        let mask_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Model backdrop coverage"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Solid model backdrop"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_body.wgsl").into()),
        });
        let geometry_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Model backdrop projection"),
                bind_group_layouts: &[Some(&geometry_layout)],
                immediate_size: 0,
            });
        let maximum = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let coverage_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Model backdrop silhouette union"),
            layout: Some(&geometry_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("body_vertex"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("body_coverage"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R8Unorm,
                    blend: Some(wgpu::BlendState {
                        color: maximum,
                        alpha: maximum,
                    }),
                    write_mask: wgpu::ColorWrites::RED,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let composite_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Model backdrop normal composition"),
            bind_group_layouts: &[Some(&mask_layout), Some(background_layout)],
            immediate_size: 0,
        });
        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Model backdrop normal composition"),
            layout: Some(&composite_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("composite_vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("composite_body"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            geometry_layout,
            mask_layout,
            coverage_pipeline,
            composite_pipeline,
            topology: Vec::new(),
            slots: Vec::new(),
            history: BodyProjectionHistory::default(),
            mask: None,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frames: &[GpuModelFrame],
        viewport: [u32; 2],
    ) {
        if viewport.contains(&0) {
            self.mask = None;
            return;
        }
        for slot in &mut self.slots {
            slot.body = None;
        }
        self.history.begin_frame();
        let mut active = 0;
        for frame in frames {
            let (Some(source), Some(projection)) = (&frame.topology.body, frame.projection) else {
                continue;
            };
            let projection = self.history.projection(frame, viewport, projection);
            if source.indices.is_empty() || frame.body_opacity <= 0.0 {
                continue;
            }
            let body = if let Some(body) = self
                .topology
                .iter()
                .find(|body| Arc::ptr_eq(&body.source, source))
            {
                Arc::clone(body)
            } else {
                let body = Arc::new(StaticBody {
                    source: Arc::clone(source),
                    vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Cached coarse model vertices"),
                        contents: bytemuck::cast_slice(&source.vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                    indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Cached coarse model triangles"),
                        contents: bytemuck::cast_slice(&source.indices),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                });
                self.topology.push(Arc::clone(&body));
                body
            };
            if active == self.slots.len() {
                let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Model backdrop live pose"),
                    size: std::mem::size_of::<BodyParams>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Model backdrop live pose"),
                    layout: &self.geometry_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    }],
                });
                self.slots.push(BodySlot {
                    body: None,
                    uniforms,
                    bindings,
                });
            }
            let slot = &mut self.slots[active];
            slot.body = Some(body);
            queue.write_buffer(
                &slot.uniforms,
                0,
                bytemuck::bytes_of(&BodyParams {
                    projection,
                    viewport: [
                        viewport[0] as f32,
                        viewport[1] as f32,
                        frame.body_opacity.clamp(0.0, 1.0),
                        0.0,
                    ],
                }),
            );
            active += 1;
        }
        self.history.finish_frame();
        // Production has at most the incoming and outgoing models. The small
        // uniform slots remain allocated through intervals with no visible body.
        self.slots.truncate(active.max(2));
        self.topology
            .retain(|body| Arc::strong_count(&body.source) > 1);
        if active == 0 {
            return;
        }
        if self.mask.as_ref().is_none_or(|mask| mask.size != viewport) {
            let view = device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("Model backdrop coverage union"),
                    size: wgpu::Extent3d {
                        width: viewport[0],
                        height: viewport[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default());
            let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Model backdrop coverage union"),
                layout: &self.mask_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            self.mask = Some(CoverageMask {
                view,
                bindings,
                size: viewport,
            });
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Model backdrop coverage union"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.mask.as_ref().unwrap().view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.coverage_pipeline);
        for slot in &self.slots {
            let Some(body) = &slot.body else {
                continue;
            };
            pass.set_bind_group(0, &slot.bindings, &[]);
            pass.set_vertex_buffer(0, body.vertices.slice(..));
            pass.set_index_buffer(body.indices.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..body.source.indices.len() as u32, 0, 0..1);
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, background: &wgpu::BindGroup) {
        let Some(mask) = &self.mask else {
            return;
        };
        if !self.slots.iter().any(|slot| slot.body.is_some()) {
            return;
        }
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, &mask.bindings, &[]);
        pass.set_bind_group(1, background, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use vello::util::{DeviceHandle, RenderContext};

    use super::*;
    use crate::visualizer::gpu_bridge::GpuTopology;

    fn fixture() -> GpuModelFrame {
        let body = GpuBodyTopology {
            vertices: vec![
                [-1.0, -1.0, 0.0, 1.0],
                [1.0, -1.0, 0.0, 1.0],
                [1.0, 1.0, 0.0, 1.0],
                [-1.0, 1.0, 0.0, 1.0],
            ],
            // Repeat both sides to exercise opacity union through overlap.
            indices: vec![0, 1, 2, 0, 2, 3, 2, 1, 0, 3, 2, 0],
        };
        let mut frame = GpuModelFrame::new(Arc::new(GpuTopology {
            body: Some(Arc::new(body)),
            edges: Vec::new(),
            vertex_count: 0,
            vertices: Vec::new(),
            bounds: [None; 2],
        }));
        frame.projection = Some(GpuProjection {
            rows: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            camera: [32.0, 32.0, 32.0, 0.5],
            depth: [2.0, 0.0, 0.0, 0.0],
            slide: [0.0; 4],
        });
        frame
    }

    fn capture(
        handle: &DeviceHandle,
        renderer: &mut GpuBodyRenderer,
        frame: &GpuModelFrame,
        background: &wgpu::BindGroup,
    ) -> anyhow::Result<Vec<u8>> {
        let target = handle.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Backdrop regression target"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());
        let readback = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Backdrop regression pixels"),
            size: 64 * 64 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        renderer.prepare(
            &handle.device,
            &handle.queue,
            &mut encoder,
            std::slice::from_ref(frame),
            [64, 64],
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Backdrop regression composition"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            renderer.draw(&mut pass, background);
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(64),
                },
            },
            target.size(),
        );
        handle.queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
        handle.device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let pixels = readback.slice(..).get_mapped_range()?.to_vec();
        readback.unmap();
        Ok(pixels)
    }

    fn pixel(pixels: &[u8], x: usize, y: usize) -> &[u8] {
        &pixels[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4]
    }

    #[test]
    #[ignore = "requires a GPU"]
    fn solid_body_unions_opacity_tracks_slide_and_clips_the_near_plane() -> anyhow::Result<()> {
        let mut context = RenderContext::new();
        let id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor::default(),
        ))?;
        let handle = &context.devices[id];
        let background_layout =
            handle
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Backdrop regression background"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    }],
                });
        let background = handle.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Backdrop regression background"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        handle.queue.write_texture(
            background.as_image_copy(),
            &[26, 51, 204, 255].repeat(64 * 64),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(64),
            },
            background.size(),
        );
        let background_bindings = handle.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Backdrop regression background"),
            layout: &background_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(
                    &background.create_view(&Default::default()),
                ),
            }],
        });
        let mut renderer = GpuBodyRenderer::new(
            &handle.device,
            wgpu::TextureFormat::Rgba8Unorm,
            &background_layout,
        );
        let mut frame = fixture();
        let opaque = capture(handle, &mut renderer, &frame, &background_bindings)?;
        assert_eq!(pixel(&opaque, 32, 32), &[26, 51, 204, 255]);
        assert_eq!(pixel(&opaque, 4, 4), &[0, 255, 0, 255]);
        let cached = Arc::clone(&renderer.topology[0]);
        frame.body_opacity = 0.5;
        let half = capture(handle, &mut renderer, &frame, &background_bindings)?;
        let expected = [13_i16, 153, 102, 255];
        for (actual, expected) in pixel(&half, 32, 32).iter().zip(expected) {
            assert!((i16::from(*actual) - expected).abs() <= 1);
        }
        assert!(Arc::ptr_eq(&cached, &renderer.topology[0]));
        assert_eq!(renderer.slots.len(), 1);
        // Increasing display times retain pose -1 even as the slide moves.
        frame.body_opacity = 1.0;
        frame.at = 1.0;
        let seeded = capture(handle, &mut renderer, &frame, &background_bindings)?;
        assert_eq!(pixel(&seeded, 20, 32), &[26, 51, 204, 255]);
        frame.at += 1.0 / 60.0;
        frame.projection.as_mut().unwrap().slide[0] = 1.0;
        let preceding = capture(handle, &mut renderer, &frame, &background_bindings)?;
        assert_eq!(pixel(&preceding, 20, 32), &[26, 51, 204, 255]);
        assert_eq!(pixel(&preceding, 56, 32), &[0, 255, 0, 255]);
        frame.at += 1.0 / 60.0;
        frame.projection.as_mut().unwrap().slide[0] = 2.0;
        let advancing = capture(handle, &mut renderer, &frame, &background_bindings)?;
        assert_eq!(pixel(&advancing, 20, 32), &[0, 255, 0, 255]);
        assert_eq!(pixel(&advancing, 56, 32), &[26, 51, 204, 255]);
        frame.body_opacity = 1.0;
        frame.projection.as_mut().unwrap().slide[0] = 1.0;
        let shifted = capture(handle, &mut renderer, &frame, &background_bindings)?;
        assert_eq!(pixel(&shifted, 20, 32), &[0, 255, 0, 255]);
        assert_eq!(pixel(&shifted, 56, 32), &[26, 51, 204, 255]);
        frame.projection.as_mut().unwrap().slide[0] = 0.0;
        frame.projection.as_mut().unwrap().rows[2] = [2.0, 0.0, 1.0, 0.0];
        let crossing = capture(handle, &mut renderer, &frame, &background_bindings)?;
        assert!(crossing.chunks_exact(4).any(|p| p == [26, 51, 204, 255]));
        frame.projection.as_mut().unwrap().depth[0] = -3.0;
        let hidden = capture(handle, &mut renderer, &frame, &background_bindings)?;
        assert!(hidden.chunks_exact(4).all(|p| p == [0, 255, 0, 255]));
        frame.body_opacity = 0.0;
        let empty = capture(handle, &mut renderer, &frame, &background_bindings)?;
        assert!(empty.chunks_exact(4).all(|p| p == [0, 255, 0, 255]));
        frame.body_opacity = 1.0;
        frame.projection = fixture().projection;
        handle.queue.write_texture(
            background.as_image_copy(),
            &[26, 51, 204, 0].repeat(64 * 64),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(64),
            },
            background.size(),
        );
        let transparent = capture(handle, &mut renderer, &frame, &background_bindings)?;
        assert!(transparent.chunks_exact(4).all(|p| p == [0, 255, 0, 255]));
        // Retiring a prepared model releases its immutable GPU geometry while
        // preserving the two small reusable pose slots and coverage texture.
        let retired = Arc::downgrade(frame.topology.body.as_ref().unwrap());
        drop(cached);
        drop(frame);
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        renderer.prepare(&handle.device, &handle.queue, &mut encoder, &[], [64, 64]);
        assert!(renderer.topology.is_empty());
        assert!(retired.upgrade().is_none());
        assert_eq!(renderer.slots.len(), 1);
        Ok(())
    }
}
