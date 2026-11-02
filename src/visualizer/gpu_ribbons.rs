//! Opt-in GPU projection and prepared-ribbon renderer.
//!
//! Topology is retained per prepared model revision. Only the two live animation
//! slots own the large preparation buffers and the three-pose projection ring.
use std::sync::Arc;

use vello::wgpu::{self, util::DeviceExt};

use super::{
    gpu_bridge::{GpuModelFrame, GpuTopology},
    ModelComposite,
};

#[cfg(test)]
mod coverage;
mod prepared;
mod projection;
use prepared::{PreparedRibbons, RibbonPipelines};
use projection::{ProjectedPose, ProjectionPipeline};

/// Minimum limits for the source models and the eight storage bindings used by
/// edge preparation. No optional adapter features or CPU readbacks are needed.
pub(super) fn supported(device: &wgpu::Device) -> bool {
    let limits = device.limits();
    limits.max_storage_buffers_per_shader_stage >= 8
        && limits.max_storage_buffer_binding_size >= 70_000 * 6 * 64
        && limits.max_buffer_size >= 70_000 * 6 * 64
        && limits.max_compute_workgroup_size_x >= 64
        && limits.max_compute_invocations_per_workgroup >= 64
}

/// Adapter capabilities which are not exposed by the created Device.
pub(super) fn adapter_supported(adapter: &wgpu::Adapter) -> bool {
    let flags = wgpu::DownlevelFlags::COMPUTE_SHADERS
        | wgpu::DownlevelFlags::INDIRECT_EXECUTION
        | wgpu::DownlevelFlags::VERTEX_STORAGE;
    let format = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba16Float);
    adapter.get_downlevel_capabilities().flags.contains(flags)
        && format
            .allowed_usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING)
        && format
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameParams {
    rotation: [[f32; 4]; 3],
    viewport: [f32; 4],
    /// Retained layout shared with the prepared-ribbon shader.
    camera: [f32; 4],
    pose: [f32; 4],
    style: [f32; 4],
    tint: [f32; 4],
    history: [u32; 4],
    /// Twice each interval's share of the actual exposure duration.
    temporal: [f32; 4],
}

struct StaticTopology {
    source: Arc<GpuTopology>,
    edges: wgpu::Buffer,
    vertices: wgpu::Buffer,
}

struct AnimationSlot {
    topology: Arc<GpuTopology>,
    reset: u64,
    viewport: [u32; 2],
    at: f64,
    newest: u32,
    uniforms: wgpu::Buffer,
    history: wgpu::Buffer,
    styles: wgpu::Buffer,
    ribbons: PreparedRibbons,
    projection: ProjectedPose,
}

impl AnimationSlot {
    fn new(
        device: &wgpu::Device,
        pipelines: Arc<RibbonPipelines>,
        projection_pipeline: &ProjectionPipeline,
        topology: &StaticTopology,
        reset: u64,
    ) -> Self {
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let uniform_usage = wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST;
        #[cfg(test)]
        let uniform_usage = uniform_usage | wgpu::BufferUsages::COPY_SRC;
        let uniforms = buffer(
            "Experimental ribbon frame parameters",
            std::mem::size_of::<FrameParams>() as u64,
            uniform_usage,
        );
        let history_usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        #[cfg(test)]
        let history_usage = history_usage | wgpu::BufferUsages::COPY_SRC;
        let history = buffer(
            "Experimental three projected poses",
            topology.source.vertex_count.max(1) as u64 * 16 * 3,
            history_usage,
        );
        let styles = buffer(
            "Experimental production edge ink",
            topology.source.edges.len().max(1) as u64 * 16,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let ribbons = PreparedRibbons::new(
            device,
            pipelines,
            &uniforms,
            &topology.edges,
            &history,
            &styles,
            topology.source.edges.len() as u32,
        );
        let projection = projection_pipeline.bind(device, &topology.vertices, &history);
        Self {
            topology: Arc::clone(&topology.source),
            reset,
            viewport: [0, 0],
            at: f64::NEG_INFINITY,
            newest: 2,
            uniforms,
            history,
            styles,
            ribbons,
            projection,
        }
    }

    fn update(
        &mut self,
        queue: &wgpu::Queue,
        viewport: [u32; 2],
        model_render_extent: [u32; 2],
        frame: &GpuModelFrame,
    ) {
        let stride = frame.topology.vertex_count as u64 * 16;
        let discontinuity = self.viewport != viewport
            || self.reset != frame.reset
            || !self.at.is_finite()
            || !frame.at.is_finite()
            || frame.at <= self.at
            || frame.at - self.at > 0.25;
        if discontinuity {
            self.newest = 2;
            // First frame after a switch/seek/resize seeds the entire ring
            // before edge preparation, so no stale motion can be exposed.
        } else {
            self.newest = (self.newest + 1) % 3;
        }
        if let Some(projection) = &frame.projection {
            self.projection.update(
                queue,
                projection,
                frame.topology.vertex_count as u32,
                self.newest,
                discontinuity,
            );
        } else {
            // Frozen screen-space fixtures and benchmark controls only.
            let bytes = bytemuck::cast_slice(&frame.projected);
            if discontinuity {
                for index in 0..3 {
                    queue.write_buffer(&self.history, index * stride, bytes);
                }
            } else {
                queue.write_buffer(&self.history, u64::from(self.newest) * stride, bytes);
            }
        }
        queue.write_buffer(&self.styles, 0, bytemuck::cast_slice(&frame.styles));
        let durations = frame.durations;
        let total = durations[0] + durations[1];
        let weights = if total.is_finite() && durations.iter().all(|v| *v > 0.0) {
            [
                2.0 * durations[0] / total,
                2.0 * durations[1] / total,
                0.0,
                0.0,
            ]
        } else {
            [1.0, 1.0, 0.0, 0.0]
        };
        queue.write_buffer(
            &self.uniforms,
            0,
            bytemuck::bytes_of(&FrameParams {
                rotation: [[0.0; 4]; 3],
                viewport: [
                    viewport[0] as f32,
                    viewport[1] as f32,
                    viewport[0] as f32 / model_render_extent[0].max(1) as f32,
                    viewport[1] as f32 / model_render_extent[1].max(1) as f32,
                ],
                camera: [0.0; 4],
                pose: [0.0; 4],
                style: [
                    frame.width,
                    1.0,
                    frame.motion_fade_power,
                    frame.motion_opacity,
                ],
                tint: [1.0; 4],
                history: [
                    frame.topology.vertex_count as u32,
                    self.newest,
                    (self.newest + 1) % 3,
                    0,
                ],
                temporal: weights,
            }),
        );
        self.viewport = viewport;
        self.reset = frame.reset;
        self.at = frame.at;
    }
}

pub(super) struct GpuRibbonRenderer {
    pipelines: Arc<RibbonPipelines>,
    projection_pipeline: ProjectionPipeline,
    topology: Vec<StaticTopology>,
    slots: Vec<AnimationSlot>,
    retired: Vec<AnimationSlot>,
}

impl GpuRibbonRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            pipelines: Arc::new(RibbonPipelines::new(device)),
            projection_pipeline: ProjectionPipeline::new(device),
            topology: Vec::new(),
            slots: Vec::new(),
            retired: Vec::new(),
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        viewport: [u32; 2],
        model_render_extent: [u32; 2],
        models: &[GpuModelFrame],
    ) {
        // Remove a slot by identity before processing the next one: a model
        // moving from current to outgoing preserves its own projection history.
        let mut previous = std::mem::take(&mut self.retired);
        std::mem::swap(&mut previous, &mut self.slots);
        for frame in models {
            if frame.topology.edges.is_empty() || frame.topology.vertex_count == 0 {
                continue;
            }
            let existing = previous
                .iter()
                .position(|slot| {
                    Arc::ptr_eq(&slot.topology, &frame.topology) && slot.reset == frame.reset
                })
                .or_else(|| {
                    previous.iter().position(|slot| {
                        // Reuse allocation after a history reset; update reseeds the
                        // ring before it can expose data from the previous generation.
                        Arc::ptr_eq(&slot.topology, &frame.topology)
                            && !models.iter().any(|other| other.reset == slot.reset)
                    })
                });
            let mut slot = if let Some(index) = existing {
                previous.remove(index)
            } else {
                let index = if let Some(index) = self
                    .topology
                    .iter()
                    .position(|entry| Arc::ptr_eq(&entry.source, &frame.topology))
                {
                    index
                } else {
                    let edges = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Experimental cached source edge topology"),
                        contents: bytemuck::cast_slice(&frame.topology.edges),
                        usage: wgpu::BufferUsages::STORAGE,
                    });
                    let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Experimental cached source vertices and slide membership"),
                        contents: if frame.topology.vertices.is_empty() {
                            bytemuck::bytes_of(&[0.0_f32; 4])
                        } else {
                            bytemuck::cast_slice(&frame.topology.vertices)
                        },
                        usage: wgpu::BufferUsages::STORAGE,
                    });
                    self.topology.push(StaticTopology {
                        source: Arc::clone(&frame.topology),
                        edges,
                        vertices,
                    });
                    self.topology.len() - 1
                };
                AnimationSlot::new(
                    device,
                    Arc::clone(&self.pipelines),
                    &self.projection_pipeline,
                    &self.topology[index],
                    frame.reset,
                )
            };
            slot.update(queue, viewport, model_render_extent, frame);
            if frame.projection.is_some() {
                self.projection_pipeline.encode(
                    encoder,
                    &slot.projection,
                    frame.topology.vertex_count as u32,
                );
            }
            slot.ribbons.encode(encoder, None);
            self.slots.push(slot);
        }
        previous.clear();
        self.retired = previous;
        // Dropping retired slots releases their large working buffers. Keep
        // static topology while its CPU prepared revision is still referenced.
        self.topology
            .retain(|entry| Arc::strong_count(&entry.source) > 1);
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, composite: ModelComposite) {
        for slot in &self.slots {
            slot.ribbons.draw(pass, composite);
        }
    }

    /// Diagnostic readback after benchmark completion, never used by playback.
    #[cfg(test)]
    pub(super) fn benchmark_coverage(
        &self,
        handle: &vello::util::DeviceHandle,
    ) -> anyhow::Result<serde_json::Value> {
        coverage::measure(self, handle)
    }
}

#[cfg(test)]
mod projection_tests;
#[cfg(test)]
mod trial_tests;
