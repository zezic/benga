use vello::{kurbo::Size, util::DeviceHandle, wgpu};

use super::{Frame, LineInstance, ModelComposite};

mod adaptive_resolution;
mod gpu_feedback;
use adaptive_resolution::AdaptiveResolution;
use gpu_feedback::GpuFeedback;
#[cfg(test)]
mod adaptive_tests;
#[cfg(test)]
mod presentation_tests;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    viewport: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayUniforms {
    opacity: [f32; 4],
}

struct TextureBinding {
    source: wgpu::TextureView,
    bindings: wgpu::BindGroup,
}

/// Accumulate all layers at display resolution before a
/// single conversion to the surface format. Repeated 8-bit blends lose faint
/// shutter contributions, even when the individual ribbon geometry is correct.
struct GpuComposition {
    renderer: Box<Renderer>,
    target: wgpu::TextureView,
    resolve: TextureBinding,
    size: [u32; 2],
}

struct RibbonLayer {
    target: wgpu::TextureView,
    resolve: TextureBinding,
    size: [u32; 2],
}

impl RibbonLayer {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, size: [u32; 2]) -> Self {
        let target = GpuComposition::target(device, size);
        let resolve = TextureBinding::new(device, layout, &target);
        Self {
            target,
            resolve,
            size,
        }
    }
}

impl GpuComposition {
    fn new(device: &wgpu::Device, resolve_layout: &wgpu::BindGroupLayout, size: [u32; 2]) -> Self {
        let target = Self::target(device, size);
        let resolve = TextureBinding::new(device, resolve_layout, &target);
        Self {
            renderer: Box::new(Renderer::new(device, wgpu::TextureFormat::Rgba16Float)),
            target,
            resolve,
            size,
        }
    }

    fn target(device: &wgpu::Device, size: [u32; 2]) -> wgpu::TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("Visualizer full-resolution float accumulation"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn resize(
        &mut self,
        device: &wgpu::Device,
        resolve_layout: &wgpu::BindGroupLayout,
        size: [u32; 2],
    ) {
        if self.size != size {
            self.target = Self::target(device, size);
            self.resolve = TextureBinding::new(device, resolve_layout, &self.target);
            self.size = size;
        }
    }
}

impl TextureBinding {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        source: &wgpu::TextureView,
    ) -> Self {
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Visualizer retained texture"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(source),
            }],
        });
        Self {
            source: source.clone(),
            bindings,
        }
    }
}

/// Composites the GPU model, retained UI face, and diagnostic lines.
pub struct Renderer {
    gpu_composition: Option<GpuComposition>,
    gpu_body: Option<super::gpu_body::GpuBodyRenderer>,
    format: wgpu::TextureFormat,
    gpu_ribbons: Option<super::gpu_ribbons::GpuRibbonRenderer>,
    adaptive_resolution: AdaptiveResolution,
    gpu_feedback: GpuFeedback,
    gpu_budget: Option<std::time::Duration>,
    frame_end_timestamp: Option<wgpu::QuerySet>,
    frame_model_timestamps: Option<wgpu::QuerySet>,
    model_downsize: f32,
    last_ribbon_extent: Option<[u32; 2]>,
    ribbon_layer: Option<RibbonLayer>,
    ribbon_upsample_additive: wgpu::RenderPipeline,
    ribbon_upsample_multiply: wgpu::RenderPipeline,
    fixed_ribbon_downsize: Option<f32>,
    background_pipeline: wgpu::RenderPipeline,
    overlay_layout: wgpu::BindGroupLayout,
    overlay_normal_pipeline: wgpu::RenderPipeline,
    overlay_additive_pipeline: wgpu::RenderPipeline,
    overlay_multiply_pipeline: wgpu::RenderPipeline,
    top_overlay_pipeline: wgpu::RenderPipeline,
    overlay_opacity: wgpu::Buffer,
    background_layout: wgpu::BindGroupLayout,
    background: Option<TextureBinding>,
    top_overlay: Option<TextureBinding>,
    line_pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    uniform_bindings: wgpu::BindGroup,
    uploaded_viewport: Option<[f32; 4]>,
    line_buffer: wgpu::Buffer,
    line_capacity: usize,
    #[cfg(test)]
    benchmark_timestamps: Option<(wgpu::QuerySet, u32, u32)>,
    #[cfg(test)]
    benchmark_resolve_timestamps: Option<(wgpu::QuerySet, u32, u32)>,
    #[cfg(test)]
    benchmark_freeze_gpu: bool,
    #[cfg(test)]
    benchmark_fill_override: Option<(wgpu::RenderPipeline, u32)>,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let background_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Visualizer background"),
            source: wgpu::ShaderSource::Wgsl(include_str!("background.wgsl").into()),
        });
        let background_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Visualizer background"),
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
        let background_pipeline = fullscreen_pipeline(
            device,
            format,
            "Visualizer background",
            &background_shader,
            &background_layout,
            "background_vertex",
            "background_fragment",
            None,
        );
        let upsample_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Visualizer model nearest-neighbour upsample"),
            source: wgpu::ShaderSource::Wgsl(include_str!("renderer/ribbon_upsample.wgsl").into()),
        });
        let upsample_pipeline = |label, blend| {
            fullscreen_pipeline_with_mask(
                device,
                format,
                label,
                &upsample_shader,
                &background_layout,
                "upsample_vertex",
                "upsample_fragment",
                Some(blend),
                wgpu::ColorWrites::COLOR,
            )
        };
        let ribbon_upsample_additive =
            upsample_pipeline("Visualizer additive model upsample", additive_blend());
        let ribbon_upsample_multiply = upsample_pipeline(
            "Visualizer multiplicative model upsample",
            multiplicative_blend(),
        );
        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Visualizer overlay"),
            source: wgpu::ShaderSource::Wgsl(include_str!("overlay.wgsl").into()),
        });
        let overlay_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Visualizer overlay"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<OverlayUniforms>() as u64,
                        ),
                    },
                    count: None,
                },
            ],
        });
        let overlay_opacity = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Visualizer overlay opacity"),
            size: std::mem::size_of::<OverlayUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let overlay_normal_pipeline = fullscreen_pipeline(
            device,
            format,
            "Visualizer overlay alpha",
            &overlay_shader,
            &overlay_layout,
            "overlay_vertex",
            "overlay_color_fragment",
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        let overlay_additive_pipeline = fullscreen_pipeline(
            device,
            format,
            "Visualizer overlay additive",
            &overlay_shader,
            &overlay_layout,
            "overlay_vertex",
            "overlay_color_fragment",
            Some(additive_blend()),
        );
        let overlay_multiply_pipeline = fullscreen_pipeline(
            device,
            format,
            "Visualizer overlay multiplicative",
            &overlay_shader,
            &overlay_layout,
            "overlay_vertex",
            "overlay_multiply_fragment",
            Some(multiplicative_blend()),
        );
        let top_overlay_pipeline = fullscreen_pipeline(
            device,
            format,
            "Visualizer top overlay",
            &overlay_shader,
            &background_layout,
            "overlay_vertex",
            "overlay_unscaled_fragment",
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Visualizer uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Uniforms>() as u64),
                },
                count: None,
            }],
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Visualizer uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Visualizer uniforms"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });

        let line_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Visualizer lines"),
            source: wgpu::ShaderSource::Wgsl(include_str!("lines.wgsl").into()),
        });
        let line_attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 32,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 48,
                shader_location: 3,
            },
        ];
        let line_vertex_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &line_attributes,
        })];
        let line_pipeline = geometry_pipeline(
            device,
            format,
            "Visualizer lines",
            &line_shader,
            "line_vertex",
            "line_fragment",
            &uniform_layout,
            &line_vertex_buffers,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            wgpu::ColorWrites::ALL,
        );
        let line_capacity = 256;
        let line_buffer = create_instance_buffer::<LineInstance>(device, line_capacity);
        Self {
            gpu_composition: None,
            gpu_body: None,
            format,
            gpu_ribbons: None,
            adaptive_resolution: AdaptiveResolution::default(),
            gpu_feedback: GpuFeedback::default(),
            gpu_budget: None,
            frame_end_timestamp: None,
            frame_model_timestamps: None,
            model_downsize: 1.0,
            last_ribbon_extent: None,
            ribbon_layer: None,
            ribbon_upsample_additive,
            ribbon_upsample_multiply,
            fixed_ribbon_downsize: None,
            background_pipeline,
            overlay_layout,
            overlay_normal_pipeline,
            overlay_additive_pipeline,
            overlay_multiply_pipeline,
            top_overlay_pipeline,
            overlay_opacity,
            background_layout,
            background: None,
            top_overlay: None,
            line_pipeline,
            uniforms,
            uniform_bindings,
            uploaded_viewport: None,
            line_buffer,
            line_capacity,
            #[cfg(test)]
            benchmark_timestamps: None,
            #[cfg(test)]
            benchmark_resolve_timestamps: None,
            #[cfg(test)]
            benchmark_freeze_gpu: false,
            #[cfg(test)]
            benchmark_fill_override: None,
        }
    }

    /// Validate GPU model requirements before starting playback or export.
    pub fn gpu_ribbons_supported(device: &wgpu::Device) -> bool {
        super::gpu_ribbons::supported(device)
    }

    pub fn gpu_ribbons_adapter_supported(adapter: &wgpu::Adapter) -> bool {
        super::gpu_ribbons::adapter_supported(adapter)
    }

    /// Adapt only the GPU model layer. Scene/UI textures stay at the physical
    /// viewport resolution.
    pub fn set_adaptive_ribbon_resolution(&mut self, enabled: bool) {
        self.adaptive_resolution.enable(enabled);
        self.gpu_feedback.restart();
        self.gpu_budget = None;
    }

    pub fn gpu_timing_tag(&mut self, viewport: [u32; 2]) -> crate::render::GpuFrameTag {
        if self.gpu_feedback.observe_viewport(viewport) {
            self.adaptive_resolution.restart();
            self.gpu_budget = None;
        }
        crate::render::GpuFrameTag {
            downsize: self.ribbon_downsize(),
            viewport,
            generation: self.gpu_feedback.generation(),
        }
    }

    pub fn set_frame_end_timestamp(&mut self, queries: Option<&wgpu::QuerySet>) {
        self.frame_end_timestamp = queries.cloned();
    }

    pub fn record_gpu_frame_timing(
        &mut self,
        timing: &crate::render::GpuFrameTiming,
        target: std::time::Duration,
    ) {
        if self.gpu_budget.is_some_and(|previous| previous != target) {
            self.reset_frame_timing();
            self.gpu_budget = Some(target);
            return;
        }
        self.gpu_budget = Some(target);
        if self.last_ribbon_extent.is_some()
            && self.fixed_ribbon_downsize.is_none()
            && self.gpu_feedback.same_session(timing)
        {
            // Each result carries the quality it measured. Ignore queued
            // frames at an older size instead of repeatedly cutting based on
            // a stale expensive frame. Presentation waiting is outside this
            // GPU command span and cannot drive resolution changes.
            let scaling_limit = self.gpu_feedback.scaling_limit(timing);
            if let Some(limit) = scaling_limit {
                if self.ribbon_downsize() > limit {
                    self.adaptive_resolution.cap_downsize(limit);
                    return;
                }
            }
            if !self.gpu_feedback.matches(timing, self.ribbon_downsize()) {
                return;
            }
            if let Some(cost) = self.gpu_feedback.control_cost(timing, target) {
                self.adaptive_resolution
                    .record_gpu_cost(cost, timing.model_elapsed, target);
                if let Some(limit) = scaling_limit {
                    self.adaptive_resolution.cap_downsize(limit);
                }
            }
        }
    }

    /// Isolated completion feedback for offscreen comparisons. Native playback
    /// uses the asynchronous, quality-tagged GPU measurement above.
    pub fn record_frame_timing(
        &mut self,
        interval: std::time::Duration,
        work: std::time::Duration,
        target: std::time::Duration,
    ) {
        if self.last_ribbon_extent.is_some() && self.fixed_ribbon_downsize.is_none() {
            self.adaptive_resolution.record(interval, work, target);
        }
    }

    /// Surface acquisition is a pacing/backpressure signal, not scalable CPU
    /// render work. Preserve it in the HUD while separating control feedback.
    pub fn record_frame_timing_with_acquisition(
        &mut self,
        interval: std::time::Duration,
        work: std::time::Duration,
        acquisition_wait: std::time::Duration,
        target: std::time::Duration,
    ) {
        if self.last_ribbon_extent.is_some() && self.fixed_ribbon_downsize.is_none() {
            self.adaptive_resolution.record_with_acquisition(
                interval,
                work,
                acquisition_wait,
                target,
            );
        }
    }

    pub fn reset_frame_timing(&mut self) {
        self.adaptive_resolution.restart();
        self.gpu_feedback.restart();
        self.gpu_budget = None;
    }

    /// Linear reduction requested for the next GPU frame.
    pub fn ribbon_downsize(&self) -> f32 {
        if let Some(downsize) = self.fixed_ribbon_downsize {
            return downsize;
        }
        self.adaptive_resolution.downsize()
    }

    /// Actual model-layer dimensions from the last rendered GPU frame.
    pub fn ribbon_render_extent(&self) -> Option<[u32; 2]> {
        self.last_ribbon_extent
    }

    /// Optional fixed quality for controlled profiling. Playback otherwise
    /// follows the adaptive controller; no saved preference is changed.
    pub fn set_fixed_ribbon_downsize(&mut self, downsize: Option<f32>) {
        self.fixed_ribbon_downsize = downsize.map(|value| {
            assert!(value.is_finite() && value >= 1.0);
            value
        });
        self.gpu_feedback.restart();
    }

    #[cfg(test)]
    pub(super) fn benchmark_ribbon_downsize(&mut self, downsize: Option<f32>) {
        self.set_fixed_ribbon_downsize(downsize);
    }

    /// Time the next render pass without changing the production rendering path.
    #[cfg(test)]
    pub(super) fn benchmark_timestamps(
        &mut self,
        queries: &wgpu::QuerySet,
        beginning: u32,
        end: u32,
    ) {
        self.benchmark_timestamps = Some((queries.clone(), beginning, end));
    }

    /// Retain already prepared three-pose ribbons for repeated draw-only
    /// capacity measurements. Seed all three poses before freezing; otherwise
    /// the ordinary frame update would turn a frozen sweep into a static line.
    #[cfg(test)]
    pub(super) fn benchmark_freeze_gpu(&mut self, frozen: bool) {
        self.benchmark_freeze_gpu = frozen;
    }

    /// Substitute a minimal full-screen fill for model drawing to distinguish
    /// ribbon shader work from float blending/attachment throughput. All other
    /// production composition and final resolve passes stay intact.
    #[cfg(test)]
    pub(super) fn benchmark_fill_override(
        &mut self,
        pipeline: Option<(wgpu::RenderPipeline, u32)>,
    ) {
        self.benchmark_fill_override = pipeline;
    }

    /// Time the next float-to-surface resolve. Together with
    /// `benchmark_timestamps`, the first beginning and this end enclose the
    /// complete frozen GPU render, including both passes and their transition.
    #[cfg(test)]
    pub(super) fn benchmark_resolve_timestamps(
        &mut self,
        queries: &wgpu::QuerySet,
        beginning: u32,
        end: u32,
    ) {
        self.benchmark_resolve_timestamps = Some((queries.clone(), beginning, end));
    }

    /// Read exact prepared primitive support after a benchmark's completion.
    /// This diagnostic is excluded from playback and every reported timing.
    #[cfg(test)]
    pub(super) fn benchmark_coverage(
        &self,
        handle: &DeviceHandle,
    ) -> anyhow::Result<serde_json::Value> {
        if let Some(composition) = &self.gpu_composition {
            return composition.renderer.benchmark_coverage(handle);
        }
        if let Some(ribbons) = &self.gpu_ribbons {
            return ribbons.benchmark_coverage(handle);
        }
        Ok(serde_json::json!({"models":[],"total_screen_layers":0.0,
            "scope":"No prepared GPU ribbon records."}))
    }

    fn ensure_line_capacity(&mut self, device: &wgpu::Device, count: usize) {
        if count <= self.line_capacity {
            return;
        }
        self.line_capacity = count.next_power_of_two();
        self.line_buffer = create_instance_buffer::<LineInstance>(device, self.line_capacity);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &DeviceHandle,
        encoder: &mut wgpu::CommandEncoder,
        background: &wgpu::TextureView,
        target: &wgpu::TextureView,
        viewport: Size,
        frame: Frame<'_>,
        overlay: Option<&wgpu::TextureView>,
        overlay_composite: Option<ModelComposite>,
        overlay_opacity: f32,
        poi_overlay: Option<&wgpu::TextureView>,
        top_overlay: Option<&wgpu::TextureView>,
    ) {
        let size = [viewport.width as u32, viewport.height as u32];
        if size.contains(&0) {
            return;
        }
        let model_extent = self.adaptive_resolution.extent(size);
        let model_extent = self.fixed_ribbon_downsize.map_or(model_extent, |downsize| {
            adaptive_resolution::extent(size, downsize)
        });
        self.last_ribbon_extent = Some(model_extent);
        let model_downsize = self.ribbon_downsize();
        let composition = self.gpu_composition.get_or_insert_with(|| {
            GpuComposition::new(&device.device, &self.background_layout, size)
        });
        composition.resize(&device.device, &self.background_layout, size);
        composition.renderer.model_downsize = model_downsize;
        composition.renderer.frame_model_timestamps = self.frame_end_timestamp.clone();
        #[cfg(test)]
        {
            composition.renderer.benchmark_timestamps = self.benchmark_timestamps.take();
        }
        #[cfg(test)]
        {
            composition.renderer.benchmark_freeze_gpu = self.benchmark_freeze_gpu;
            composition.renderer.benchmark_fill_override = self.benchmark_fill_override.clone();
        }
        composition.renderer.draw_layers(
            device,
            encoder,
            background,
            &composition.target,
            viewport,
            frame,
            overlay,
            overlay_composite,
            overlay_opacity,
            poi_overlay,
            top_overlay,
        );
        #[cfg(test)]
        let resolve_timestamps = self.benchmark_resolve_timestamps.take();
        let frame_end_timestamp = self.frame_end_timestamp.take();
        let timestamp_writes =
            frame_end_timestamp
                .as_ref()
                .map(|queries| wgpu::RenderPassTimestampWrites {
                    query_set: queries,
                    beginning_of_pass_write_index: None,
                    end_of_pass_write_index: Some(3),
                });
        #[cfg(test)]
        let timestamp_writes = timestamp_writes.or_else(|| {
            resolve_timestamps.as_ref().map(|(set, beginning, end)| {
                wgpu::RenderPassTimestampWrites {
                    query_set: set,
                    beginning_of_pass_write_index: Some(*beginning),
                    end_of_pass_write_index: Some(*end),
                }
            })
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Visualizer float accumulation resolve"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            timestamp_writes,
            ..Default::default()
        });
        pass.set_pipeline(&self.background_pipeline);
        pass.set_bind_group(0, &composition.resolve.bindings, &[]);
        pass.draw(0..3, 0..1);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_layers(
        &mut self,
        device: &DeviceHandle,
        encoder: &mut wgpu::CommandEncoder,
        background: &wgpu::TextureView,
        target: &wgpu::TextureView,
        viewport: Size,
        frame: Frame<'_>,
        overlay: Option<&wgpu::TextureView>,
        overlay_composite: Option<ModelComposite>,
        overlay_opacity: f32,
        poi_overlay: Option<&wgpu::TextureView>,
        top_overlay: Option<&wgpu::TextureView>,
    ) {
        let prepare_gpu = true;
        #[cfg(test)]
        let prepare_gpu = prepare_gpu && !self.benchmark_freeze_gpu;
        let physical_extent = [viewport.width as u32, viewport.height as u32];
        let model_extent = adaptive_resolution::extent(physical_extent, self.model_downsize);
        // Keep the model in its own float layer even at 1x, so GPU timing
        // measures the same work at every adaptive quality.
        if prepare_gpu {
            self.gpu_ribbons
                .get_or_insert_with(|| super::gpu_ribbons::GpuRibbonRenderer::new(&device.device))
                .prepare(
                    &device.device,
                    &device.queue,
                    encoder,
                    physical_extent,
                    model_extent,
                    frame.gpu_models,
                );
        }
        if frame
            .gpu_models
            .iter()
            .any(|model| model.topology.body.is_some())
            || self.gpu_body.is_some()
        {
            self.gpu_body
                .get_or_insert_with(|| {
                    super::gpu_body::GpuBodyRenderer::new(
                        &device.device,
                        self.format,
                        &self.background_layout,
                    )
                })
                .prepare(
                    &device.device,
                    &device.queue,
                    encoder,
                    frame.gpu_models,
                    physical_extent,
                );
        }
        if self
            .background
            .as_ref()
            .is_none_or(|binding| binding.source != *background)
        {
            self.background = Some(TextureBinding::new(
                &device.device,
                &self.background_layout,
                background,
            ));
        }
        let viewport = [viewport.width as f32, viewport.height as f32, 0.0, 0.0];
        if self.uploaded_viewport != Some(viewport) {
            device.queue.write_buffer(
                &self.uniforms,
                0,
                bytemuck::bytes_of(&Uniforms { viewport }),
            );
            self.uploaded_viewport = Some(viewport);
        }
        let line_count = frame.lines.len();
        self.ensure_line_capacity(&device.device, line_count.max(2048));
        if line_count > 0 {
            device
                .queue
                .write_buffer(&self.line_buffer, 0, bytemuck::cast_slice(frame.lines));
        }
        if overlay.is_some() || poi_overlay.is_some() {
            device.queue.write_buffer(
                &self.overlay_opacity,
                0,
                bytemuck::bytes_of(&OverlayUniforms {
                    opacity: [overlay_opacity.clamp(0.0, 2.0), 0.0, 0.0, 0.0],
                }),
            );
        }
        let bind_overlay = |view: &wgpu::TextureView| {
            device.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Visualizer foreground overlay"),
                layout: &self.overlay_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.overlay_opacity.as_entire_binding(),
                    },
                ],
            })
        };
        let overlay_binding = overlay.map(bind_overlay);
        let poi_binding = poi_overlay.map(bind_overlay);
        if let Some(view) = top_overlay {
            if self
                .top_overlay
                .as_ref()
                .is_none_or(|binding| binding.source != *view)
            {
                self.top_overlay = Some(TextureBinding::new(
                    &device.device,
                    &self.background_layout,
                    view,
                ));
            }
        }
        #[cfg(test)]
        let benchmark_timestamps = self.benchmark_timestamps.take();
        let frame_model_timestamps = self.frame_model_timestamps.take();
        {
            if self
                .ribbon_layer
                .as_ref()
                .is_none_or(|layer| layer.size != model_extent)
            {
                self.ribbon_layer = Some(RibbonLayer::new(
                    &device.device,
                    &self.background_layout,
                    model_extent,
                ));
            }
            let neutral = match frame.model_composite {
                ModelComposite::Additive => wgpu::Color::TRANSPARENT,
                ModelComposite::Multiplicative => wgpu::Color::WHITE,
            };
            let timestamp_writes =
                frame_model_timestamps
                    .as_ref()
                    .map(|queries| wgpu::RenderPassTimestampWrites {
                        query_set: queries,
                        beginning_of_pass_write_index: Some(1),
                        end_of_pass_write_index: Some(2),
                    });
            #[cfg(test)]
            let timestamp_writes = timestamp_writes.or_else(|| {
                benchmark_timestamps.as_ref().map(|(set, beginning, _)| {
                    wgpu::RenderPassTimestampWrites {
                        query_set: set,
                        beginning_of_pass_write_index: Some(*beginning),
                        end_of_pass_write_index: None,
                    }
                })
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Visualizer GPU ribbon layer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.ribbon_layer.as_ref().unwrap().target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(neutral),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                timestamp_writes,
                ..Default::default()
            });
            self.draw_gpu_ribbons(&mut pass, frame.model_composite);
        }
        let timestamp_writes = None;
        #[cfg(test)]
        let timestamp_writes = timestamp_writes.or_else(|| {
            benchmark_timestamps
                .as_ref()
                .map(|(set, _, end)| wgpu::RenderPassTimestampWrites {
                    query_set: set,
                    beginning_of_pass_write_index: None,
                    end_of_pass_write_index: Some(*end),
                })
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Visualizer overlay"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            timestamp_writes,
            ..Default::default()
        });
        pass.set_pipeline(&self.background_pipeline);
        pass.set_bind_group(
            0,
            &self
                .background
                .as_ref()
                .expect("the retained background has been bound")
                .bindings,
            &[],
        );
        pass.draw(0..3, 0..1);
        if let Some(binding) = &overlay_binding {
            let pipeline = match overlay_composite {
                None => &self.overlay_normal_pipeline,
                Some(ModelComposite::Additive) => &self.overlay_additive_pipeline,
                Some(ModelComposite::Multiplicative) => &self.overlay_multiply_pipeline,
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, binding, &[]);
            pass.draw(0..3, 0..1);
        }
        if let Some(body) = &self.gpu_body {
            body.draw(&mut pass, &self.background.as_ref().unwrap().bindings);
        }
        {
            let pipeline = match frame.model_composite {
                ModelComposite::Additive => &self.ribbon_upsample_additive,
                ModelComposite::Multiplicative => &self.ribbon_upsample_multiply,
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(
                0,
                &self.ribbon_layer.as_ref().unwrap().resolve.bindings,
                &[],
            );
            pass.draw(0..3, 0..1);
        }
        if line_count > 0 {
            pass.set_pipeline(&self.line_pipeline);
            pass.set_bind_group(0, &self.uniform_bindings, &[]);
            pass.set_vertex_buffer(0, self.line_buffer.slice(..));
            pass.draw(0..6, 0..line_count as u32);
        }
        if let Some(binding) = &poi_binding {
            pass.set_pipeline(&self.overlay_normal_pipeline);
            pass.set_bind_group(0, binding, &[]);
            pass.draw(0..3, 0..1);
        }
        if top_overlay.is_some() {
            let binding = self
                .top_overlay
                .as_ref()
                .expect("the top overlay texture has been bound");
            pass.set_pipeline(&self.top_overlay_pipeline);
            pass.set_bind_group(0, &binding.bindings, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    fn draw_gpu_ribbons(&self, pass: &mut wgpu::RenderPass<'_>, composite: ModelComposite) {
        #[cfg(test)]
        if let Some((pipeline, count)) = &self.benchmark_fill_override {
            pass.set_pipeline(pipeline);
            pass.draw(0..3, 0..*count);
            return;
        }
        if let Some(ribbons) = &self.gpu_ribbons {
            ribbons.draw(pass, composite);
        }
    }
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    label: &str,
    shader: &wgpu::ShaderModule,
    texture_layout: &wgpu::BindGroupLayout,
    vertex_entry: &str,
    fragment_entry: &str,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    fullscreen_pipeline_with_mask(
        device,
        format,
        label,
        shader,
        texture_layout,
        vertex_entry,
        fragment_entry,
        blend,
        wgpu::ColorWrites::ALL,
    )
}

#[allow(clippy::too_many_arguments)]
fn fullscreen_pipeline_with_mask(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    label: &str,
    shader: &wgpu::ShaderModule,
    texture_layout: &wgpu::BindGroupLayout,
    vertex_entry: &str,
    fragment_entry: &str,
    blend: Option<wgpu::BlendState>,
    write_mask: wgpu::ColorWrites,
) -> wgpu::RenderPipeline {
    let layouts = [Some(texture_layout)];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &layouts,
                immediate_size: 0,
            }),
        ),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex_entry),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn additive_blend() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

fn multiplicative_blend() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Dst,
            dst_factor: wgpu::BlendFactor::Zero,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::Zero,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn geometry_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    label: &str,
    shader: &wgpu::ShaderModule,
    vertex_entry: &str,
    fragment_entry: &str,
    uniform_layout: &wgpu::BindGroupLayout,
    vertex_buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
    blend: wgpu::BlendState,
    write_mask: wgpu::ColorWrites,
) -> wgpu::RenderPipeline {
    let layouts = [Some(uniform_layout)];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &layouts,
                immediate_size: 0,
            }),
        ),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex_entry),
            compilation_options: Default::default(),
            buffers: vertex_buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend),
                write_mask,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_instance_buffer<T>(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Visualizer instance buffer"),
        size: capacity as u64 * std::mem::size_of::<T>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::BufWriter, sync::mpsc};

    use vello::{kurbo::Size, util::RenderContext, wgpu};

    use super::*;
    use crate::{
        audio::timeline::{AudioInterval, AudioTime},
        config::ShutterFrames,
        theme::Theme,
        visualizer::{ModelMotion, Visualizer},
    };

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn top_overlay_keeps_normal_alpha_across_foreground_modes() -> anyhow::Result<()> {
        let mut context = RenderContext::new();
        let id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor::default(),
        ))?;
        let device = &context.devices[id];
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let width = 64;
        let textures = ["Background", "Foreground", "Top overlay", "Target"].map(|label| {
            device.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        });
        let [background, foreground, top_overlay, target] = &textures;
        for (texture, pixels) in [
            (background, [0, 0, 255, 255].repeat(width as usize)),
            (foreground, [0, 200, 0, 128].repeat(width as usize)),
            (
                top_overlay,
                [255, 0, 0, 128]
                    .repeat(width as usize / 2)
                    .into_iter()
                    .chain([0, 0, 0, 0].repeat(width as usize / 2))
                    .collect(),
            ),
        ] {
            device.queue.write_texture(
                texture.as_image_copy(),
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(1),
                },
                texture.size(),
            );
        }
        let [background, foreground, top_overlay, target_view] = textures
            .each_ref()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Top overlay readback"),
            size: u64::from(width * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut renderer = Renderer::new(&device.device, format);
        for composite in [
            None,
            Some(ModelComposite::Additive),
            Some(ModelComposite::Multiplicative),
        ] {
            for opacity in [0.0, 1.0] {
                let mut encoder = device
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                renderer.draw(
                    device,
                    &mut encoder,
                    &background,
                    &target_view,
                    Size::new(f64::from(width), 1.0),
                    Frame {
                        lines: &[],
                        gpu_models: &[],
                        model_composite: ModelComposite::Additive,
                    },
                    Some(&foreground),
                    composite,
                    opacity,
                    None,
                    Some(&top_overlay),
                );
                encoder.copy_texture_to_buffer(
                    target.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &readback,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(width * 4),
                            rows_per_image: Some(1),
                        },
                    },
                    target.size(),
                );
                device.queue.submit([encoder.finish()]);
                let (tx, rx) = mpsc::channel();
                readback
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
                device.device.poll(wgpu::PollType::wait_indefinitely())?;
                rx.recv()??;
                let pixels = readback.slice(..).get_mapped_range()?.to_vec();
                readback.unmap();
                let covered = &pixels[..4];
                let uncovered = &pixels[width as usize * 2..width as usize * 2 + 4];
                for channel in 0..3 {
                    let ink = if channel == 0 { 255.0 } else { 0.0 };
                    let expected = (ink * 128.0 + f32::from(uncovered[channel]) * 127.0) / 255.0;
                    assert!(
                        (f32::from(covered[channel]) - expected).abs() <= 1.0,
                        "top overlay must retain normal alpha at foreground opacity {opacity}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn gpu_composition_preserves_edge_styles_and_faint_ink() -> anyhow::Result<()> {
        use std::sync::Arc;

        use crate::visualizer::gpu_bridge::{GpuModelFrame, GpuTopology};

        let mut context = RenderContext::new();
        let id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor::default(),
        ))?;
        let device = &context.devices[id];
        assert!(Renderer::gpu_ribbons_supported(&device.device));
        let width = 64;
        let height = 16;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = |label, usage| {
            device.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let background = texture(
            "GPU bridge test background",
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let target = texture(
            "GPU bridge test display target",
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let background_view = background.create_view(&Default::default());
        let target_view = target.create_view(&Default::default());
        let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU bridge test readback"),
            size: u64::from(width * height * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut edges = vec![[0, 1], [2, 3]];
        edges.extend(std::iter::repeat_n([4, 5], 32));
        let mut styles = vec![[1.0, 0.0, 0.0, 0.7], [0.0, 1.0, 0.0, 0.7]];
        // Each contribution is below an 8-bit blend step. Together they must
        // remain visible through float accumulation and the final conversion.
        styles.extend(std::iter::repeat_n([0.0, 0.0, 1.0, 1.0 / 1024.0], 32));
        let model = GpuModelFrame {
            body_opacity: 1.0,
            topology: Arc::new(GpuTopology {
                body: None,
                edges,
                vertex_count: 6,
                vertices: Vec::new(),
                bounds: [None; 2],
            }),
            projected: vec![
                [16.0, 3.0, 1.0, 1.0],
                [16.0, 13.0, 1.0, 1.0],
                [32.0, 3.0, 1.0, 1.0],
                [32.0, 13.0, 1.0, 1.0],
                [48.0, 3.0, 1.0, 1.0],
                [48.0, 13.0, 1.0, 1.0],
            ],
            projection: None,
            styles,
            width: 3.0,
            motion_fade_power: 0.5,
            motion_opacity: 0.0,
            at: 1.0,
            reset: 1,
            durations: [1.0 / 60.0; 2],
        };
        let mut renderer = Renderer::new(&device.device, format);
        for composite in [ModelComposite::Additive, ModelComposite::Multiplicative] {
            let level = match composite {
                ModelComposite::Additive => 0,
                ModelComposite::Multiplicative => 255,
            };
            device.queue.write_texture(
                background.as_image_copy(),
                &[level, level, level, 255].repeat((width * height) as usize),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                background.size(),
            );
            let mut encoder = device.device.create_command_encoder(&Default::default());
            renderer.draw(
                device,
                &mut encoder,
                &background_view,
                &target_view,
                Size::new(f64::from(width), f64::from(height)),
                Frame {
                    lines: &[],
                    gpu_models: std::slice::from_ref(&model),
                    model_composite: composite,
                },
                None,
                None,
                1.0,
                None,
                None,
            );
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(width * 4),
                        rows_per_image: Some(height),
                    },
                },
                target.size(),
            );
            device.queue.submit([encoder.finish()]);
            let (tx, rx) = mpsc::channel();
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
            device.device.poll(wgpu::PollType::wait_indefinitely())?;
            rx.recv()??;
            let pixels = readback.slice(..).get_mapped_range()?.to_vec();
            readback.unmap();
            let pixel = |x| &pixels[(8 * width as usize + x) * 4..][..4];
            assert!(
                pixel(16)[0] > pixel(16)[1] + 50,
                "red edge: {:?}",
                pixel(16)
            );
            assert!(
                pixel(32)[1] > pixel(32)[0] + 50,
                "green edge: {:?}",
                pixel(32)
            );
            let faint = pixel(48);
            assert!(
                faint[0].abs_diff(level) > 2 || faint[2].abs_diff(level) > 2,
                "sub-8-bit contributions must accumulate: {faint:?}"
            );
            let accumulation = renderer.gpu_composition.as_ref().unwrap();
            assert_eq!(accumulation.size, [width, height]);
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_gpu_wireframe_and_swept_motion() -> anyhow::Result<()> {
        let mut context = RenderContext::new();
        let id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor::default(),
        ))?;
        let device = &context.devices[id];
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let width = 1280;
        let height = 440;
        let background = device.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Visualizer test background"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let colors = Theme::new(crate::theme::ThemeId::default()).colors;
        let palette = crate::visualizer::Palette::from_theme(crate::theme::ThemeId::default());
        let background_pixels: Vec<u8> = (0..width * height)
            .flat_map(|_| {
                colors.background.components[..3]
                    .iter()
                    .map(|value| (value * 255.0) as u8)
                    .chain(std::iter::once(255))
            })
            .collect();
        device.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &background,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &background_pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let target = device.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Visualizer test target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let background_view = background.create_view(&wgpu::TextureViewDescriptor::default());
        let mut renderer = Renderer::new(&device.device, format);
        let mut visualizer = Visualizer::new();
        visualizer.set_samplerate(48_000);
        visualizer.set_model_motion(ModelMotion::Swept);
        visualizer.load_models();
        let startup_frame = visualizer.frame();
        let mut encoder = device
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        renderer.draw(
            device,
            &mut encoder,
            &background_view,
            &target_view,
            Size::new(f64::from(width), f64::from(height)),
            startup_frame,
            None,
            None,
            1.0,
            None,
            None,
        );
        device.queue.submit([encoder.finish()]);
        let mut at = AudioTime::ZERO;
        for frame in 0..180 {
            let mut input = Vec::with_capacity(800);
            for index in 0..800 {
                let sample = frame * 800 + index;
                let time = sample as f32 / 48_000.0;
                let kick = if (0..10).contains(&(sample % 9_600)) {
                    0.9
                } else {
                    0.0
                };
                let tone = 0.25 * (std::f32::consts::TAU * 1_000.0 * time).sin()
                    + 0.16 * (std::f32::consts::TAU * 220.0 * time).sin();
                let kick_tone = kick * (std::f32::consts::TAU * 64.0 * time).sin();
                input.push([tone + kick_tone; 2]);
            }
            visualizer.consume_buf(&input, 1.0);
            let end = at.shifted(800.0 / 48_000.0);
            visualizer.update(
                AudioInterval::new(at, end),
                ShutterFrames::default(),
                800.0 / 48_000.0,
                &palette,
                (width as f32, height as f32),
                1.0,
                1.0,
            );
            at = end;
            // Seed the GPU's two preceding displayed poses before capturing
            // the last pose; animation updates alone do not render history.
            if (177..179).contains(&frame) {
                let mut encoder = device.device.create_command_encoder(&Default::default());
                renderer.draw(
                    device,
                    &mut encoder,
                    &background_view,
                    &target_view,
                    Size::new(f64::from(width), f64::from(height)),
                    visualizer.frame(),
                    None,
                    None,
                    1.0,
                    None,
                    None,
                );
                device.queue.submit([encoder.finish()]);
            }
        }
        let frame = visualizer.frame();
        assert!(
            !frame.gpu_models.is_empty(),
            "animation must provide GPU projection parameters and ink"
        );
        let mut encoder = device
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        renderer.draw(
            device,
            &mut encoder,
            &background_view,
            &target_view,
            Size::new(f64::from(width), f64::from(height)),
            frame,
            None,
            None,
            1.0,
            None,
            None,
        );
        let row_bytes = width * 4;
        let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Visualizer test readback"),
            size: u64::from(row_bytes * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(height),
                },
            },
            target.size(),
        );
        device.queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
        device.device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let pixels = readback.slice(..).get_mapped_range()?.to_vec();
        readback.unmap();
        let ink = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(background_pixels.as_chunks::<4>().0)
            .filter(|(pixel, background)| {
                pixel[..3]
                    .iter()
                    .zip(&background[..3])
                    .any(|(pixel, background)| pixel.abs_diff(*background) > 24)
            })
            .count();
        eprintln!(
            "visualizer ink pixels: {ink}; {} diagnostic lines, {} GPU models",
            frame.lines.len(),
            frame.gpu_models.len()
        );
        assert!(ink > 2_000, "the wireframe must cover the retained face");
        if let Some(path) = std::env::var_os("BENGA_RENDER_PREVIEW_PATH") {
            let path = std::path::PathBuf::from(path).with_extension("visualizer.png");
            let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&pixels)?;
        }
        Ok(())
    }
}
