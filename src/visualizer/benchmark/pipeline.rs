//! Full retained-UI and model pipeline, without a window or display waits.
//!
//! Run `cargo test --release measures_full_presentation_pipeline -- --ignored --nocapture --test-threads=1`.
//! BENGA_PIPELINE_BENCH_DIR, BENGA_PIPELINE_FRAMES, BENGA_PIPELINE_WARMUP and
//! BENGA_PIPELINE_EXTENTS / BENGA_PIPELINE_FPS customize the bounded workload. Each extent is physical
//! pixels. Default 5120x2880 represents the measured backing extent of the user's
//! 3840x2160 display in its scaled mode; it is not inferred from UI scale.
//! BENGA_PIPELINE_WORKLOAD selects normal/poi_static/poi_flight/zoom_stress;
//! zoom_stress uses BENGA_PIPELINE_ZOOM_NEAR/FAR/MIN/MAX/HZ camera inputs.
//! BENGA_PIPELINE_MAX_FRAME_MS / MIN_PEAK_MS enforce a red-capable timing verdict
//! after results are saved (full frame completion, excluding query readback).
//! BENGA_PIPELINE_COMPONENTS selects full/no_model/no_foreground/no_gpu_prepare/
//! no_model_no_foreground. no_gpu_prepare retains ribbons prepared by the first
//! three GPU frames while CPU animation/UI continue; it requires GPU ribbons,
//! at least three warmup frames and a fixed resolution policy.
//! BENGA_PIPELINE_CONFIG_PATH reads a Config without modifying it. Optional
//! BENGA_PIPELINE_CPU_WARMUP advances complete UI state before GPU warmup.
//! BENGA_PIPELINE_PREVIEW_FRAMES captures comma-separated, zero-based measured
//! frame indices as PNGs after rendering, outside every reported frame cost.
//! Production GPU projection and ribbon rendering are always enabled, with
//! a fixed two-frame shutter when motion blur is enabled.
//! GPU runs retain full resolution by default. BENGA_PIPELINE_ADAPTIVE_RIBBONS=1
//! replays isolated completion costs into the live resolution controller, using
//! BENGA_PIPELINE_FPS as its target. BENGA_PIPELINE_RIBBON_DOWNSIZE fixes a linear
//! downsize factor for matched image/performance comparisons instead.
//! BENGA_PIPELINE_RIBBON_DOWNSIZES sweeps comma-separated fixed factors with
//! fresh, identical workload setup per case; it excludes the singular/adaptive knobs.
//! BENGA_PIPELINE_COVERAGE=1 reads the last frame's prepared raster support
//! outside timing, for asset budgeting; no playback readback is introduced.
//! BENGA_PIPELINE_MODEL selects an asset ID for normal/zoom runs; the default
//! selects the most detailed loaded model.

use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{ensure, Context};
use serde_json::json;
use strum::EnumCount;
use vello::{
    kurbo::{Affine, Rect, Size},
    peniko::{Color, Fill},
    util::{DeviceHandle, RenderContext},
    wgpu, RenderParams, Scene,
};

use crate::{
    config::Config,
    frame_schedule::{Adjustment, FrameRateLimit, FrameTime},
    render::{
        BackgroundCache, Frame as RetainedFrame, PlateMask, RasterAssetCache, SolidBackground,
    },
    AudioEvent,
};

use super::super::{
    Models, Property, PropertyGates, Renderer, Smoother, Ui, UiFrame, View, BAND_COUNT,
};
use super::{poi_workload, summary};

use super::zoom_workload;

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
enum WorkloadMode {
    Normal,
    PoiStatic,
    PoiFlight,
    ZoomStress,
}

impl WorkloadMode {
    fn from_env() -> anyhow::Result<Self> {
        match std::env::var("BENGA_PIPELINE_WORKLOAD")
            .as_deref()
            .unwrap_or("normal")
        {
            "normal" => Ok(Self::Normal),
            "poi_static" => Ok(Self::PoiStatic),
            "poi_flight" => Ok(Self::PoiFlight),
            "zoom_stress" => Ok(Self::ZoomStress),
            other => anyhow::bail!("unknown pipeline workload {other}"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::PoiStatic => "poi_static",
            Self::PoiFlight => "poi_flight",
            Self::ZoomStress => "zoom_stress",
        }
    }

    fn poi(self) -> Option<poi_workload::Mode> {
        match self {
            Self::Normal | Self::ZoomStress => None,
            Self::PoiStatic => Some(poi_workload::Mode::Static),
            Self::PoiFlight => Some(poi_workload::Mode::Flight),
        }
    }
}

#[derive(Clone, Copy)]
enum Components {
    Full,
    NoModel,
    NoForeground,
    NoGpuPrepare,
    NoModelNoForeground,
}

impl Components {
    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::NoModel => "no_model",
            Self::NoForeground => "no_foreground",
            Self::NoGpuPrepare => "no_gpu_prepare",
            Self::NoModelNoForeground => "no_model_no_foreground",
        }
    }

    fn omits_model(self) -> bool {
        matches!(self, Self::NoModel | Self::NoModelNoForeground)
    }

    fn omits_foreground(self) -> bool {
        matches!(self, Self::NoForeground | Self::NoModelNoForeground)
    }

    fn freezes_gpu_preparation(self) -> bool {
        matches!(self, Self::NoGpuPrepare)
    }
}

fn components() -> anyhow::Result<Vec<Components>> {
    std::env::var("BENGA_PIPELINE_COMPONENTS")
        .unwrap_or_else(|_| "full".into())
        .split(',')
        .map(|name| match name.trim() {
            "full" => Ok(Components::Full),
            "no_model" => Ok(Components::NoModel),
            "no_foreground" => Ok(Components::NoForeground),
            "no_gpu_prepare" => Ok(Components::NoGpuPrepare),
            "no_model_no_foreground" => Ok(Components::NoModelNoForeground),
            other => anyhow::bail!("unknown pipeline component policy {other}"),
        })
        .collect()
}

fn base_config() -> anyhow::Result<Config> {
    match std::env::var_os("BENGA_PIPELINE_CONFIG_PATH") {
        Some(path) => {
            let path = PathBuf::from(path);
            let source = fs::read_to_string(&path)
                .with_context(|| format!("could not read pipeline config {}", path.display()))?;
            ron::from_str(&source).context("could not parse pipeline Config")
        }
        None => Ok(Config::default()),
    }
}

struct Pipeline {
    context: RenderContext,
    device_id: usize,
    renderer: vello::Renderer,
    model_renderer: Renderer,
    background_cache: BackgroundCache,
    solid_background: SolidBackground,
    assets: RasterAssetCache,
    plate_mask: PlateMask,
    _textures: Vec<wgpu::Texture>,
    target: wgpu::Texture,
    background_view: wgpu::TextureView,
    foreground_view: wgpu::TextureView,
    poi_view: wgpu::TextureView,
    controls_view: wgpu::TextureView,
    target_view: wgpu::TextureView,
    background_ready: bool,
    params: RenderParams,
    queries: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    readback: wgpu::Buffer,
    timestamp_period_ns: f64,
}

struct RenderTime {
    cpu_encode_ms: f64,
    completion_wait_ms: f64,
    model_gpu_ms: f64,
    background_rasterized: bool,
}

impl Pipeline {
    fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        let mut context = RenderContext::new();
        let device_id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor {
                required_features: wgpu::Features::TIMESTAMP_QUERY,
                ..Default::default()
            },
        ))?;
        let handle = &context.devices[device_id];
        ensure!(
            handle
                .device
                .features()
                .contains(wgpu::Features::TIMESTAMP_QUERY),
            "GPU timestamps unsupported"
        );
        let make = |label, format, usage| {
            handle.device.create_texture(&wgpu::TextureDescriptor {
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
        let layer_usage = wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT;
        let background = make(
            "Pipeline retained background",
            wgpu::TextureFormat::Rgba8Unorm,
            layer_usage,
        );
        let foreground = make(
            "Pipeline animated foreground",
            wgpu::TextureFormat::Rgba8Unorm,
            layer_usage,
        );
        let poi = make(
            "Pipeline POI callouts",
            wgpu::TextureFormat::Rgba8Unorm,
            layer_usage,
        );
        let controls = make(
            "Pipeline controls",
            wgpu::TextureFormat::Rgba8Unorm,
            layer_usage,
        );
        let target = make(
            "Pipeline final target",
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let background_view = background.create_view(&Default::default());
        let foreground_view = foreground.create_view(&Default::default());
        let poi_view = poi.create_view(&Default::default());
        let controls_view = controls.create_view(&Default::default());
        let target_view = target.create_view(&Default::default());
        let mut renderer = crate::render::renderer(&handle.device)?;
        let plate_mask = PlateMask::new(&handle.device, &mut renderer, width, height);
        let model_renderer = Renderer::new(&handle.device, wgpu::TextureFormat::Bgra8Unorm);
        let queries = handle.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("Pipeline model pass timestamp"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let resolved = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pipeline timestamp resolve"),
            size: 16,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pipeline timestamp readback"),
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let timestamp_period_ns = f64::from(handle.queue.get_timestamp_period());
        Ok(Self {
            context,
            device_id,
            renderer,
            model_renderer,
            background_cache: BackgroundCache::new(),
            solid_background: SolidBackground::default(),
            assets: RasterAssetCache::new(),
            plate_mask,
            _textures: vec![background, foreground, poi, controls],
            target,
            background_view,
            foreground_view,
            poi_view,
            controls_view,
            target_view,
            background_ready: false,
            params: RenderParams {
                base_color: Color::TRANSPARENT,
                width,
                height,
                antialiasing_method: crate::render::ANTIALIASING,
            },
            queries,
            resolved,
            readback,
            timestamp_period_ns,
        })
    }

    fn handle(&self) -> &DeviceHandle {
        &self.context.devices[self.device_id]
    }

    fn adapter(&self) -> String {
        let info = self.handle().adapter().get_info();
        format!(
            "{}; {:?}; {:?}; physical {}x{}; Rgba8Unorm layers, Bgra8Unorm target",
            info.name, info.backend, info.device_type, self.params.width, self.params.height
        )
    }

    fn wait(&self) -> anyhow::Result<()> {
        self.handle().device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(TIMEOUT),
        })?;
        Ok(())
    }

    fn write_preview(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let handle = self.handle();
        let row_bytes = self.params.width * 4;
        let padded = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pipeline preview readback"),
            size: u64::from(padded) * u64::from(self.params.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.params.height),
                },
            },
            self.target.size(),
        );
        handle.queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        self.wait()?;
        rx.recv_timeout(TIMEOUT)
            .context("pipeline preview readback timed out")??;
        let mut pixels = Vec::with_capacity((row_bytes * self.params.height) as usize);
        {
            let mapped = readback.slice(..).get_mapped_range()?;
            for row in mapped.chunks_exact(padded as usize) {
                pixels.extend_from_slice(&row[..row_bytes as usize]);
            }
        }
        readback.unmap();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(fs::File::create(path)?),
            self.params.width,
            self.params.height,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&pixels)?;
        writer.finish()?;
        Ok(())
    }

    fn render(
        &mut self,
        frame: UiFrame<'_>,
        foreground_scene: &Scene,
        controls_scene: &Scene,
        controls_visible: bool,
        force_background: bool,
        components: Components,
    ) -> anyhow::Result<RenderTime> {
        let before = Instant::now();
        let handle = &self.context.devices[self.device_id];
        let mut forced_solid = Scene::new();
        let background = if force_background {
            match frame.background {
                RetainedFrame::Solid(color) => {
                    // Keep the explicit benchmark control for measuring the
                    // former vector-raster path against the production clear.
                    forced_solid.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        color,
                        None,
                        &Rect::new(
                            0.0,
                            0.0,
                            f64::from(self.params.width),
                            f64::from(self.params.height),
                        ),
                    );
                    RetainedFrame::Rebuilt(&forced_solid)
                }
                RetainedFrame::Rebuilt(scene)
                | RetainedFrame::Retained(scene)
                | RetainedFrame::Unchanged(scene) => RetainedFrame::Rebuilt(scene),
            }
        } else {
            frame.background
        };
        let solid_color = if let RetainedFrame::Solid(color) = background {
            Some(color)
        } else {
            None
        };
        let background_rasterized = solid_color.is_none()
            && (matches!(background, RetainedFrame::Rebuilt(_)) || !self.background_ready);
        if matches!(background, RetainedFrame::Rebuilt(_)) {
            self.background_cache
                .benchmark_invalidate(&mut self.renderer);
        }
        let foreground_enabled = !components.omits_foreground();
        if foreground_enabled && !frame.plate_instances.is_empty() {
            self.plate_mask.render(
                &handle.device,
                &handle.queue,
                frame.plate_instances,
                frame.plate_ink,
            );
            self.renderer
                .mark_override_image_dirty(&self.plate_mask.image().image);
        }
        if foreground_enabled {
            self.assets
                .prepare(&mut self.renderer, handle, &frame.raster_assets)?;
        }
        if background_rasterized {
            let scene = self.background_cache.compose(
                &mut self.renderer,
                handle,
                background,
                &self.params,
            )?;
            self.renderer.render_to_texture(
                &handle.device,
                &handle.queue,
                scene,
                &self.background_view,
                &self.params,
            )?;
            self.background_ready = true;
        }
        if foreground_enabled && frame.foreground {
            self.renderer.render_to_texture(
                &handle.device,
                &handle.queue,
                foreground_scene,
                &self.foreground_view,
                &self.params,
            )?;
        }
        let poi_overlay = frame.poi_overlay.filter(|_| foreground_enabled);
        if let Some(poi_scene) = poi_overlay {
            self.renderer.render_to_texture(
                &handle.device,
                &handle.queue,
                poi_scene,
                &self.poi_view,
                &self.params,
            )?;
        }
        if controls_visible {
            self.renderer.render_to_texture(
                &handle.device,
                &handle.queue,
                controls_scene,
                &self.controls_view,
                &self.params,
            )?;
        }
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        let background_view = if let Some(color) = solid_color {
            self.solid_background
                .prepare(&handle.device, &mut encoder, color, &self.params)
        } else {
            &self.background_view
        };
        self.model_renderer
            .benchmark_timestamps(&self.queries, 0, 1);
        let model_overlay = if components.omits_model() {
            super::super::Frame {
                lines: frame.overlay.lines,
                model_composite: frame.overlay.model_composite,
                // Omit geometry while preserving float composition and resolve.
                gpu_models: &[],
            }
        } else {
            frame.overlay
        };
        self.model_renderer.draw(
            handle,
            &mut encoder,
            background_view,
            &self.target_view,
            Size::new(f64::from(self.params.width), f64::from(self.params.height)),
            model_overlay,
            (foreground_enabled && frame.foreground).then_some(&self.foreground_view),
            frame.overlay_composite,
            frame.overlay_opacity,
            poi_overlay.map(|_| &self.poi_view),
            controls_visible.then_some(&self.controls_view),
        );
        handle.queue.submit([encoder.finish()]);
        let cpu_encode_ms = before.elapsed().as_secs_f64() * 1000.0;
        let wait_start = Instant::now();
        self.wait()?;
        let completion_wait_ms = wait_start.elapsed().as_secs_f64() * 1000.0;
        // Counter resolution follows host-observed completion on Metal. This
        // diagnostic readback is outside the frame-completion metric below.
        let handle = self.handle();
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        encoder.resolve_query_set(&self.queries, 0..2, &self.resolved, 0);
        encoder.copy_buffer_to_buffer(&self.resolved, 0, &self.readback, 0, 16);
        handle.queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        self.wait()?;
        rx.recv_timeout(TIMEOUT)
            .context("pipeline timestamp callback timed out")??;
        let model_gpu_ms = {
            let mapped = self.readback.slice(..).get_mapped_range()?;
            let begin = u64::from_ne_bytes(mapped[..8].try_into().unwrap());
            let end = u64::from_ne_bytes(mapped[8..16].try_into().unwrap());
            ensure!(
                begin > 0 && end >= begin && end != u64::MAX,
                "invalid pipeline model timestamp {begin}/{end}"
            );
            (end - begin) as f64 * self.timestamp_period_ns / 1e6
        };
        self.readback.unmap();
        Ok(RenderTime {
            cpu_encode_ms,
            completion_wait_ms,
            model_gpu_ms,
            background_rasterized,
        })
    }
}

fn select_benchmark_model(ui: &mut Ui) -> anyhow::Result<(String, usize)> {
    let visualizer = ui.benchmark_visualizer_mut();
    visualizer.load_models();
    let Models::Ready(models) = &mut visualizer.models else {
        anyhow::bail!("models did not load")
    };
    let mut largest = (String::new(), 0);
    let requested = std::env::var("BENGA_PIPELINE_MODEL").ok();
    for _ in 0..models.position().1 {
        let model = models.current();
        let count = model.wireframe(visualizer.crease_threshold).edges().len();
        if requested
            .as_ref()
            .map_or(count > largest.1, |id| *id == model.id)
        {
            largest = (model.id.clone(), count);
        }
        models.navigate(Adjustment::Increase);
    }
    ensure!(
        !largest.0.is_empty(),
        "requested benchmark model was not found: {:?}",
        requested
    );
    for _ in 0..models.position().1 {
        if models.current().id == largest.0 {
            break;
        }
        models.navigate(Adjustment::Increase);
    }
    visualizer.scene.show(
        models.current_prepared(),
        visualizer.model_motion,
        0.5,
        &visualizer.style_tuning,
    );
    visualizer.sync_showcase();
    Ok(largest)
}

fn controlled_motion(ui: &mut Ui, time: f32) {
    let visualizer = ui.benchmark_visualizer_mut();
    visualizer.pending_actions.clear();
    let mut muted = [true; Property::COUNT];
    muted[Property::Line.index()] = false;
    visualizer.property_gates = PropertyGates {
        muted,
        soloed: [false; Property::COUNT],
    };
    visualizer.debug_music([0.8; BAND_COUNT], 0.8);
    if let Some(model) = &mut visualizer.scene.current {
        model.pose.yaw = 0.5 + 2.0 * (time * 5.0).sin();
        model.pose.pitch = 0.0;
        model.pitch = Smoother::new(0.0);
        model.pose.roll = 0.0;
        model.roll = Smoother::new(0.0);
    }
}

fn extents() -> anyhow::Result<Vec<(u32, u32)>> {
    std::env::var("BENGA_PIPELINE_EXTENTS")
        .unwrap_or_else(|_| "3840x2160,5120x2880".into())
        .split(',')
        .map(|extent| {
            let (width, height) = extent
                .split_once('x')
                .context("extent must be WIDTHxHEIGHT")?;
            Ok((width.parse()?, height.parse()?))
        })
        .collect()
}

#[test]
#[ignore = "requires a GPU; measures complete presentation rasterization and composition"]
fn measures_full_presentation_pipeline() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var("BENGA_PIPELINE_BENCH_DIR")
            .unwrap_or_else(|_| "target/pipeline-benchmark".into()),
    );
    fs::create_dir_all(&directory)?;
    let fps: u32 = std::env::var("BENGA_PIPELINE_FPS")
        .unwrap_or_else(|_| "60".into())
        .parse()?;
    ensure!((1..=480).contains(&fps), "FPS must be in 1..=480");
    let measured: usize = std::env::var("BENGA_PIPELINE_FRAMES")
        .unwrap_or_else(|_| "60".into())
        .parse()?;
    let warmup: usize = std::env::var("BENGA_PIPELINE_WARMUP")
        .unwrap_or_else(|_| "120".into())
        .parse()?;
    let cpu_warmup: usize = std::env::var("BENGA_PIPELINE_CPU_WARMUP")
        .unwrap_or_else(|_| "0".into())
        .parse()?;
    ensure!(
        measured > 0 && measured <= 1200 && warmup <= 1200 && cpu_warmup <= 4800,
        "frame counts exceed bounded benchmark limits"
    );
    let force_background = std::env::var_os("BENGA_PIPELINE_FORCE_BACKGROUND").is_some();
    let capture_coverage = std::env::var("BENGA_PIPELINE_COVERAGE").as_deref() == Ok("1");
    let adaptive_ribbons = match std::env::var("BENGA_PIPELINE_ADAPTIVE_RIBBONS").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("0" | "false") => false,
        Ok("1" | "true") => true,
        _ => anyhow::bail!("BENGA_PIPELINE_ADAPTIVE_RIBBONS must be 0, 1, false, or true"),
    };
    let fixed_downsize = std::env::var("BENGA_PIPELINE_RIBBON_DOWNSIZE")
        .ok()
        .map(|value| -> anyhow::Result<f32> {
            let factor: f32 = value
                .parse()
                .context("invalid BENGA_PIPELINE_RIBBON_DOWNSIZE")?;
            ensure!(
                factor.is_finite() && factor >= 1.0,
                "BENGA_PIPELINE_RIBBON_DOWNSIZE must be finite and at least 1"
            );
            Ok(factor)
        })
        .transpose()?;
    let fixed_downsizes = std::env::var("BENGA_PIPELINE_RIBBON_DOWNSIZES")
        .ok()
        .map(|value| -> anyhow::Result<Vec<f32>> {
            let factors = value
                .split(',')
                .map(|value| {
                    let factor: f32 = value
                        .trim()
                        .parse()
                        .context("invalid BENGA_PIPELINE_RIBBON_DOWNSIZES")?;
                    ensure!(
                        factor.is_finite() && factor >= 1.0,
                        "BENGA_PIPELINE_RIBBON_DOWNSIZES factors must be finite and at least 1"
                    );
                    Ok(factor)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            ensure!(
                !factors.is_empty() && factors.len() <= 32,
                "BENGA_PIPELINE_RIBBON_DOWNSIZES requires 1..=32 factors"
            );
            Ok(factors)
        })
        .transpose()?;
    ensure!(
        !adaptive_ribbons || fixed_downsize.is_none(),
        "choose either adaptive ribbon resolution or a fixed downsize"
    );
    ensure!(
        fixed_downsizes.is_none() || (!adaptive_ribbons && fixed_downsize.is_none()),
        "BENGA_PIPELINE_RIBBON_DOWNSIZES excludes the singular downsize and adaptive resolution options"
    );
    let quality_cases: Vec<Option<f32>> = fixed_downsizes.as_ref().map_or_else(
        || vec![fixed_downsize],
        |factors| factors.iter().copied().map(Some).collect(),
    );
    let workload_mode = WorkloadMode::from_env()?;
    let zoom_workload = matches!(workload_mode, WorkloadMode::ZoomStress)
        .then(zoom_workload::ZoomWorkload::from_env)
        .transpose()?;
    let timing_limit = |name: &str| -> anyhow::Result<Option<f64>> {
        std::env::var(name)
            .ok()
            .map(|value| {
                let value: f64 = value.parse().with_context(|| format!("invalid {name}"))?;
                ensure!(
                    value.is_finite() && value > 0.0,
                    "{name} must be positive and finite"
                );
                Ok(value)
            })
            .transpose()
    };
    let max_frame_ms = timing_limit("BENGA_PIPELINE_MAX_FRAME_MS")?;
    let min_peak_ms = timing_limit("BENGA_PIPELINE_MIN_PEAK_MS")?;
    let components = components()?;
    if components
        .iter()
        .any(|policy| policy.freezes_gpu_preparation())
    {
        ensure!(
            !adaptive_ribbons && warmup >= 3,
            "no_gpu_prepare requires GPU ribbons, a fixed resolution policy and at least three GPU warmup frames"
        );
    }
    let base_config = base_config()?;
    let preview_frames: Vec<usize> = std::env::var("BENGA_PIPELINE_PREVIEW_FRAMES")
        .unwrap_or_default()
        .split(',')
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().parse())
        .collect::<Result<_, _>>()?;
    let mut cases = Vec::new();
    for ((width, height), fixed_downsize) in extents()?.into_iter().flat_map(|extent| {
        quality_cases
            .iter()
            .copied()
            .map(move |downsize| (extent, downsize))
    }) {
        for &components in &components {
            let mut config = base_config.clone();
            config.model_rotation = super::super::ModelRotation::Locked;
            config.frame_rate_limit = match fps {
                15 => FrameRateLimit::Fps15,
                30 => FrameRateLimit::Fps30,
                48 => FrameRateLimit::Fps48,
                55 => FrameRateLimit::Fps55,
                60 => FrameRateLimit::Fps60,
                72 => FrameRateLimit::Fps72,
                90 => FrameRateLimit::Fps90,
                120 => FrameRateLimit::Fps120,
                180 => FrameRateLimit::Fps180,
                240 => FrameRateLimit::Fps240,
                _ => FrameRateLimit::VSync,
            };
            let mut ui = Ui::new(&config)?;
            ui.dismiss_startup();
            let mut view = View::fit(Size::new(f64::from(width), f64::from(height)));
            view.set_ui_scale(2.0);
            view.set_zoom(config.zoom);
            let mut pipeline = Pipeline::new(width, height)?;
            pipeline
                .model_renderer
                .set_adaptive_ribbon_resolution(adaptive_ribbons);
            pipeline
                .model_renderer
                .benchmark_ribbon_downsize(fixed_downsize);
            ensure!(
                Renderer::gpu_ribbons_supported(&pipeline.handle().device)
                    && Renderer::gpu_ribbons_adapter_supported(pipeline.handle().adapter()),
                "integrated GPU ribbons unsupported on the benchmark adapter"
            );
            let limits = pipeline.handle().device.limits();
            ui.validate_gpu_models(
                (u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size)
                    / (6 * 64)) as usize,
            )?;
            ui.set_plate_mask(Some(pipeline.plate_mask.image()));
            ui.audio_event(AudioEvent::Config { samplerate: 48_000 });
            let workload_metadata = if let Some(mode) = workload_mode.poi() {
                poi_workload::configure(&mut ui, mode)?
            } else {
                let (model, raw_edges) = select_benchmark_model(&mut ui)?;
                json!({"model_id":model,"raw_edges":raw_edges,"zoom_script":zoom_workload.as_ref().map(|script| script.metadata())})
            };
            let origin = Instant::now() + Duration::from_secs(12);
            let mut foreground = Scene::new();
            let mut controls = Scene::new();
            let mut draw_ms = Vec::new();
            let mut audio_ms = Vec::new();
            let mut controls_ms = Vec::new();
            let mut encode_ms = Vec::new();
            let mut completion_ms = Vec::new();
            let mut full_ms = Vec::new();
            let mut model_gpu_ms = Vec::new();
            let mut rasterized = 0;
            let mut ui_rebuilt = 0;
            let mut active = Vec::new();
            let mut triangles = Vec::new();
            let mut transparent_ribbons = Vec::new();
            let mut controls_frames = 0;
            let mut plate_counts = Vec::new();
            let mut plate_unclipped_area = Vec::new();
            let mut asset_counts = Vec::new();
            let mut poi_events = Vec::new();
            let mut poi_frames = Vec::new();
            let mut zoom_frames = Vec::new();
            let mut previews = Vec::new();
            let mut raster_coverage = json!(null);
            let mut projected_pose_bytes = Vec::new();
            let mut projection_parameter_bytes = Vec::new();
            let mut edge_style_bytes = Vec::new();
            let mut cpu_geometry_bytes = Vec::new();
            let mut gpu_model_counts = Vec::new();
            let mut ribbon_downsizes = Vec::new();
            let mut ribbon_render_extents = Vec::new();
            for index in 0..cpu_warmup + warmup + measured {
                let start = origin + Duration::from_secs_f64(index as f64 / f64::from(fps));
                let end = origin + Duration::from_secs_f64((index + 1) as f64 / f64::from(fps));
                let audio_frames =
                    ((index + 1) * 48_000 / fps as usize) - (index * 48_000 / fps as usize);
                let samples = vec![[0.1; 2]; audio_frames];
                let begin = Instant::now();
                ui.audio_event(AudioEvent::Buffer {
                    buf: samples,
                    captured_at: start,
                });
                let event = if let Some(mode) = workload_mode.poi() {
                    poi_workload::drive(&mut ui, index, fps, mode)
                } else if let Some(script) = &zoom_workload {
                    script.drive(&mut ui, &mut view, (index + 1) as f64 / f64::from(fps))
                } else {
                    controlled_motion(&mut ui, (index + 1) as f32 / fps as f32);
                    json!(null)
                };
                let audio_duration = begin.elapsed().as_secs_f64() * 1000.0;
                let controls_start = Instant::now();
                let controls_visible = ui.draw_controls(&mut controls, &view, &config, true, end);
                let controls_duration = controls_start.elapsed().as_secs_f64() * 1000.0;
                foreground.reset();
                let draw_start = Instant::now();
                let frame = ui.draw(
                    &mut foreground,
                    &view,
                    config.frame_rate_limit,
                    FrameTime::new(start, end),
                );
                let draw_duration = draw_start.elapsed().as_secs_f64() * 1000.0;
                let rebuilt = matches!(frame.background, RetainedFrame::Rebuilt(_));
                ensure!(
                    frame
                        .overlay
                        .gpu_models
                        .iter()
                        .all(|model| { model.projection.is_some() && model.projected.is_empty() }),
                    "production GPU path generated a CPU-projected model pose"
                );
                let projected_bytes = frame
                    .overlay
                    .gpu_models
                    .iter()
                    .map(|model| model.projected.len() * std::mem::size_of::<[f32; 4]>())
                    .sum::<usize>();
                let style_bytes = frame
                    .overlay
                    .gpu_models
                    .iter()
                    .map(|model| model.styles.len() * std::mem::size_of::<[f32; 4]>())
                    .sum::<usize>();
                let projection_bytes = frame
                    .overlay
                    .gpu_models
                    .iter()
                    .filter_map(|model| model.projection.as_ref())
                    .map(std::mem::size_of_val)
                    .sum::<usize>();
                // Projection and ribbon construction are GPU-owned; CPU
                // expanded geometry and transparent-ribbon scans are absent.
                let geometry_bytes = 0usize;
                let gpu_models = frame.overlay.gpu_models.len();
                let count = 0usize;
                let transparent = 0usize;
                let plate_count = frame.plate_instances.len();
                let plate_area: f64 = frame
                    .plate_instances
                    .iter()
                    .map(|plate| {
                        let [x0, y0, x1, y1] = plate.bounds;
                        f64::from((x1 - x0).max(0.0) * (y1 - y0).max(0.0))
                    })
                    .sum();
                let assets = frame.raster_assets.len();
                if index < cpu_warmup {
                    // Complete CPU scene/camera/history updates are retained.
                    // The first GPU frame realizes the current retained face.
                    continue;
                }
                // Keep a real three-pose ribbon history, then omit projection,
                // projection/style uploads and preparation while UI/audio move.
                // This attributes fixed render work; it is not a matched image.
                pipeline.model_renderer.benchmark_freeze_gpu(
                    components.freezes_gpu_preparation() && index - cpu_warmup >= 3,
                );
                let rendered = pipeline.render(
                    frame,
                    &foreground,
                    &controls,
                    controls_visible,
                    force_background,
                    components,
                )?;
                // Query readback is outside this sum. All actual audio, UI
                // construction, raster preparation, rendering, and waiting remain.
                let total = audio_duration
                    + controls_duration
                    + draw_duration
                    + rendered.cpu_encode_ms
                    + rendered.completion_wait_ms;
                // Capture the quality actually used before feedback chooses
                // the next frame. This is isolated completion feedback, not
                // native presentation cadence or GPU execution time.
                let ribbon_downsize = pipeline.model_renderer.ribbon_downsize();
                let ribbon_render_extent = pipeline.model_renderer.ribbon_render_extent();
                if adaptive_ribbons {
                    let completion = Duration::from_secs_f64(total / 1000.0);
                    pipeline.model_renderer.record_frame_timing(
                        completion,
                        completion,
                        Duration::from_secs_f64(1.0 / f64::from(fps)),
                    );
                }
                if capture_coverage && index + 1 == cpu_warmup + warmup + measured {
                    raster_coverage = pipeline
                        .model_renderer
                        .benchmark_coverage(pipeline.handle())?;
                    let prepared_frame = if components.freezes_gpu_preparation() {
                        cpu_warmup + 2
                    } else {
                        index
                    };
                    raster_coverage["animation_frame"] = json!(prepared_frame);
                    raster_coverage["animation_time_seconds"] =
                        json!((prepared_frame + 1) as f64 / f64::from(fps));
                    let visualizer = ui.benchmark_visualizer_mut();
                    raster_coverage["model_names"] = json!(visualizer
                        .scene
                        .current
                        .iter()
                        .chain(visualizer.scene.outgoing.iter())
                        .map(|model| model.name())
                        .collect::<Vec<_>>());
                    if let Some(slots) = raster_coverage["models"].as_array_mut() {
                        for slot in slots {
                            let generation = slot["animation_generation"].as_u64();
                            let Some(model) = visualizer
                                .scene
                                .current
                                .iter()
                                .chain(visualizer.scene.outgoing.iter())
                                .find(|model| Some(model.gpu_generation) == generation)
                            else {
                                continue;
                            };
                            slot["model_name"] = json!(model.name());
                            let asset = match &visualizer.models {
                                Models::Ready(models) if models.current().name == model.name() => {
                                    Some(models.current())
                                }
                                _ => None,
                            };
                            if let Some(asset) = asset {
                                slot["asset_id"] = json!(asset.id);
                            }
                            if let Some(edges) = slot["top_source_edges"].as_array_mut() {
                                for entry in edges {
                                    let Some(index) =
                                        entry["source_edge"].as_u64().map(|n| n as usize)
                                    else {
                                        continue;
                                    };
                                    let Some(edge) = model.wireframe.edges().get(index) else {
                                        continue;
                                    };
                                    let part = model.wireframe.edge_part(index);
                                    entry["part_id"] = json!(part);
                                    if let Some(asset) = asset {
                                        entry["part_name"] = json!(asset.part_name(part));
                                    }
                                    entry["source_model_space_endpoints"] =
                                        json!(edge.map(|vertex| {
                                            let p = model.wireframe.vertex(vertex as usize);
                                            [p.x, p.y, p.z]
                                        }));
                                }
                            }
                        }
                    }
                }
                if index >= cpu_warmup + warmup {
                    audio_ms.push(audio_duration);
                    controls_ms.push(controls_duration);
                    draw_ms.push(draw_duration);
                    encode_ms.push(rendered.cpu_encode_ms);
                    completion_ms.push(rendered.completion_wait_ms);
                    full_ms.push(total);
                    model_gpu_ms.push(rendered.model_gpu_ms);
                    rasterized += usize::from(rendered.background_rasterized);
                    ui_rebuilt += usize::from(rebuilt);
                    controls_frames += usize::from(controls_visible);
                    active.push(ui.benchmark_visualizer_mut().line_count());
                    triangles.push(count);
                    transparent_ribbons.push(transparent);
                    plate_counts.push(plate_count);
                    plate_unclipped_area.push(plate_area);
                    asset_counts.push(assets);
                    projected_pose_bytes.push(projected_bytes);
                    projection_parameter_bytes.push(projection_bytes);
                    edge_style_bytes.push(style_bytes);
                    cpu_geometry_bytes.push(geometry_bytes);
                    gpu_model_counts.push(gpu_models);
                    ribbon_downsizes.push(ribbon_downsize);
                    ribbon_render_extents.push(ribbon_render_extent);
                    poi_events.push(event);
                    poi_frames.push(if workload_mode.poi().is_some() {
                        poi_workload::snapshot(&mut ui)
                    } else {
                        json!(null)
                    });
                    zoom_frames.push(if zoom_workload.is_some() {
                        zoom_workload::snapshot(&mut ui, &view)
                    } else {
                        json!(null)
                    });
                    let measured_index = index - cpu_warmup - warmup;
                    if preview_frames.contains(&measured_index) {
                        let quality_suffix = if fixed_downsizes.is_some() {
                            format!("-downsize{}", fixed_downsize.expect("fixed sweep factor"))
                        } else {
                            String::new()
                        };
                        let filename = format!(
                            "{width}x{height}-{}-{}{quality_suffix}-frame{measured_index:04}.png",
                            workload_mode.name(),
                            components.name()
                        );
                        pipeline.write_preview(&directory.join(&filename))?;
                        previews.push(json!({"measured_frame":measured_index,"path":filename}));
                    }
                }
            }
            let (peak_index, peak_ms) = full_ms
                .iter()
                .copied()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .expect("measured frames");
            eprintln!(
                "Peak frame {peak_index}: {peak_ms:.3} ms; animation time {:.6}s",
                (cpu_warmup + warmup + peak_index + 1) as f64 / f64::from(fps)
            );
            eprintln!("{width}x{height} {} {} fixed_downsize={fixed_downsize:?}: full completion {:.3}/{:.3}ms median/p95; Ui.draw {:.3}ms; encode {:.3}ms; modelGPU {:.3}ms; background {rasterized}/{measured}",
                    workload_mode.name(),components.name(),summary(&full_ms)["median"].as_f64().unwrap(),summary(&full_ms)["p95"].as_f64().unwrap(),summary(&draw_ms)["median"].as_f64().unwrap(),summary(&encode_ms)["median"].as_f64().unwrap(),summary(&model_gpu_ms)["median"].as_f64().unwrap());
            let peak_frame = json!({"measured_index":peak_index,"animation_frame":cpu_warmup+warmup+peak_index,
                "animation_time_seconds":(cpu_warmup+warmup+peak_index+1) as f64/f64::from(fps),
                "full_completion_ms":peak_ms,"cpu_audio_ms":audio_ms[peak_index],"cpu_controls_ms":controls_ms[peak_index],
                "cpu_ui_draw_ms":draw_ms[peak_index],"cpu_raster_encode_submit_ms":encode_ms[peak_index],
                "completion_wait_ms":completion_ms[peak_index],"model_gpu_ms":model_gpu_ms[peak_index],
                "zoom":zoom_frames[peak_index]});
            let mut case = json!({
                "width":width,"height":height,"ui_scale":2.0,"fps":fps,"interval_seconds":1.0/f64::from(fps),
                "workload":workload_mode.name(),"workload_metadata":workload_metadata,"components":components.name(),
                "active_backend":if components.omits_model() {"model_omitted"} else if components.freezes_gpu_preparation() {"gpu_ribbons_frozen_projection_and_preparation"} else {"gpu_ribbons_gpu_projection"},
                "accumulation_format":"Rgba16Float",
                "model_gpu_timestamp_scope":"model rasterization and float layer composition; excludes GPU projection, edge preparation and final resolve",
                "model_payload_bytes":{
                    "projected_pose":projected_pose_bytes,"edge_style":edge_style_bytes,
                    "cpu_expanded_geometry":cpu_geometry_bytes,"active_gpu_models":gpu_model_counts,
                    "scope":"Selected per-frame CPU-built model payload. Production uploads projection parameters and edge styles; projected_pose is zero. Excludes 16-byte projection ring metadata, ribbon frame uniforms, diagnostic lines and immutable source topology/positions. no_model policies still build these CPU payloads but omit their GPU upload. no_gpu_prepare omits GPU model uploads and preparation after its first three GPU frames.",
                },
                "render_settings":{
                    "model_motion":config.model_motion,"shutter_frames":crate::config::ShutterFrames::default(),
                    "line_width":config.line_width,"zoom":config.zoom,"motion_opacity":config.motion_opacity,
                    "motion_fade_power":config.motion_fade_power,"depth_fade":config.depth_fade,
                    "near_opacity":config.near_opacity,"crease_threshold":config.crease_threshold,
                    "presentation_opacity":config.presentation_opacity,"text_scale":config.visualizer_text_scale,
                    "selected_theme":ui.selected_theme(),"requested_frame_limit":base_config.frame_rate_limit,
                    "showcase":config.tuning.showcase,
                },
                "adapter":pipeline.adapter(),
                "warmup_frames":warmup,"measured_frames":measured,"background_rasterized_frames":rasterized,
                "cpu_warmup_frames":cpu_warmup,"measurement_start_seconds":(cpu_warmup+warmup) as f64/f64::from(fps),
                "ui_face_rebuilt_frames":ui_rebuilt,"controls_visible_frames":controls_frames,
                "cpu_audio_ms":summary(&audio_ms),"cpu_ui_draw_ms":summary(&draw_ms),"cpu_controls_ms":summary(&controls_ms),
                "cpu_raster_encode_submit_ms":summary(&encode_ms),"completion_wait_ms":summary(&completion_ms),
                "full_completion_ms":summary(&full_ms),"model_gpu_ms":summary(&model_gpu_ms),
                "full_completion_samples_ms":full_ms,"draw_samples_ms":draw_ms,"encode_samples_ms":encode_ms,
                "model_gpu_samples_ms":model_gpu_ms,"active_edges":active,"triangles":triangles,
                "transparent_ribbons":transparent_ribbons,
                "plate_instances":plate_counts,"plate_unclipped_bounds_area_px2":plate_unclipped_area,
                "retained_assets":asset_counts,"poi_events":poi_events,"poi_frames":poi_frames,
                "previews":previews,
            });
            case["ribbon_resolution"] = json!({
                "adaptive":adaptive_ribbons,"fixed_downsize":fixed_downsize,
                "target_fps":fps,"linear_downsize_samples":ribbon_downsizes,
                "render_extent_samples":ribbon_render_extents,
                "feedback_scope":"Adaptive replay uses isolated full-frame completion as both interval and work; it does not measure native presentation FPS. Background, foreground, diagnostics and controls retain the physical output extent.",
            });
            case["model_payload_bytes"]["projection_parameters"] =
                json!(projection_parameter_bytes);
            case["gpu_preparation_control"] = json!({
                "frozen":components.freezes_gpu_preparation(),
                "seed_gpu_frames":if components.freezes_gpu_preparation() {Some(3)} else {None},
                "last_prepared_animation_frame":if components.freezes_gpu_preparation() {Some(cpu_warmup+2)} else {None},
                "scope":"no_gpu_prepare seeds three actual GPU poses, then draws retained ribbon records while audio, CPU Ui.draw and foreground/control rendering continue. Projection, edge preparation and model uploads are omitted after seeding; frozen geometry/ink is not image-equivalent to the animated full control.",
            });
            case["zoom_frames"] = json!(zoom_frames);
            case["peak_frame"] = peak_frame;
            case["raster_coverage"] = raster_coverage;
            cases.push(case);
        }
    }
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec_pretty(&json!({
        "cases":cases,"force_background_rebuild":force_background,"fps":fps,"workload":workload_mode.name(),
        "gpu_ribbons_requested":true,
        "adaptive_ribbons":adaptive_ribbons,"fixed_ribbon_downsize":fixed_downsize,
        "fixed_ribbon_downsizes":fixed_downsizes,
        "parallel_support":std::env::var("BENGA_GPU_RIBBON_PARALLEL_BOUNDS").unwrap_or_else(|_| "original".into()),
        "maximum_frame_budget_ms":max_frame_ms,"minimum_reproduction_peak_ms":min_peak_ms,
        "read_saved_config":std::env::var_os("BENGA_PIPELINE_CONFIG_PATH").is_some(),
            "method":"Actual Ui.draw and synthetic stereo capture with controlled normal motion or actual Showcase POI scripts. Retained background/raster-asset caches, Vello animated foreground/control passes, plate masks, themed line renderer, and final composition. Physical extents and UI scale are explicit. One frame completes before the next; no surface acquisition, display wait, native window chrome, OS compositor or VSync.",
            "component_controls":"no_model omits model geometry at GPU composition, preserving the selected backend's composition format and final resolve; edge preparation has no model work. no_foreground omits GPU plate rendering, retained foreground asset preparation, foreground rasterization and composition. no_model_no_foreground combines both omissions. no_gpu_prepare retains real ribbons after three GPU seed frames, omitting subsequent GPU projection, edge preparation and model uploads while still drawing the frozen ribbons; its images/history differ from animated controls. All policies still build the complete Ui.draw CPU scene/model.",
            "timing_scope":"full_completion_ms adds audio/script, controls, Ui.draw, render encoding/submission and GPU completion wait. It is isolated completion, not achieved application FPS. Model timestamps cover final composition, potentially including dependencies on prior Vello writes. Query readback is excluded. Model setup occurs before timing.",
        }))?,
    )?;
    // Save raw samples and exact peak-frame state before returning a failing
    // verdict, so this command can serve as a performance regression loop.
    for case in &cases {
        let peak = case["full_completion_ms"]["max"]
            .as_f64()
            .expect("measured maximum");
        if let Some(limit) = max_frame_ms {
            ensure!(
                peak <= limit,
                "{}x{} {} {} exceeded frame budget: peak {peak:.3} ms > {limit:.3} ms; see {}",
                case["width"],
                case["height"],
                case["workload"],
                case["active_backend"],
                directory.display()
            );
        }
        if let Some(minimum) = min_peak_ms {
            ensure!(peak >= minimum,
                "{}x{} {} did not reproduce requested slow frame: peak {peak:.3} ms < {minimum:.3} ms; see {}",
                case["width"],case["height"],case["workload"],directory.display());
        }
    }
    Ok(())
}
