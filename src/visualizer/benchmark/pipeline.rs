//! Full retained-UI and model pipeline, without a window or display waits.
//!
//! Run `cargo test --release measures_full_presentation_pipeline -- --ignored --nocapture --test-threads=1`.
//! BENGA_PIPELINE_BENCH_DIR, BENGA_PIPELINE_FRAMES, BENGA_PIPELINE_WARMUP and
//! BENGA_PIPELINE_EXTENTS / BENGA_PIPELINE_FPS customize the bounded workload. Each extent is physical
//! pixels. Default 5120x2880 represents the measured backing extent of the user's
//! 3840x2160 display in its scaled mode; it is not inferred from UI scale.
//! BENGA_PIPELINE_WORKLOAD selects normal/poi_static/poi_flight;
//! BENGA_PIPELINE_COMPONENTS selects full/no_model/no_foreground;
//! BENGA_PIPELINE_CONFIG_PATH reads a Config without modifying it. Optional
//! BENGA_PIPELINE_CPU_WARMUP advances complete UI state before GPU warmup.
//! BENGA_PIPELINE_PREVIEW_FRAMES captures comma-separated, zero-based measured
//! frame indices as PNGs after rendering, outside every reported frame cost.

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
    kurbo::Size,
    peniko::Color,
    util::{DeviceHandle, RenderContext},
    wgpu, RenderParams, Scene,
};

use crate::{
    config::Config,
    frame_schedule::{Adjustment, FrameRateLimit, FrameTime},
    render::{BackgroundCache, Frame as RetainedFrame, PlateMask, RasterAssetCache},
    AudioEvent,
};

use super::super::{
    Models, Property, PropertyGates, Renderer, Smoother, Ui, UiFrame, View, BAND_COUNT,
};
use super::{poi_workload, summary};

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
enum WorkloadMode {
    Normal,
    PoiStatic,
    PoiFlight,
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
            other => anyhow::bail!("unknown pipeline workload {other}"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::PoiStatic => "poi_static",
            Self::PoiFlight => "poi_flight",
        }
    }

    fn poi(self) -> Option<poi_workload::Mode> {
        match self {
            Self::Normal => None,
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
}

impl Components {
    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::NoModel => "no_model",
            Self::NoForeground => "no_foreground",
        }
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
    assets: RasterAssetCache,
    plate_mask: PlateMask,
    _textures: Vec<wgpu::Texture>,
    target: wgpu::Texture,
    background_view: wgpu::TextureView,
    foreground_view: wgpu::TextureView,
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
            assets: RasterAssetCache::new(),
            plate_mask,
            _textures: vec![background, foreground, controls],
            target,
            background_view,
            foreground_view,
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
        let background = if force_background {
            match frame.background {
                RetainedFrame::Rebuilt(scene)
                | RetainedFrame::Retained(scene)
                | RetainedFrame::Unchanged(scene) => RetainedFrame::Rebuilt(scene),
            }
        } else {
            frame.background
        };
        let background_rasterized =
            matches!(background, RetainedFrame::Rebuilt(_)) || !self.background_ready;
        if matches!(background, RetainedFrame::Rebuilt(_)) {
            self.background_cache
                .benchmark_invalidate(&mut self.renderer);
        }
        let foreground_enabled = !matches!(components, Components::NoForeground);
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
        self.model_renderer
            .benchmark_timestamps(&self.queries, 0, 1);
        let model_overlay = if matches!(components, Components::NoModel) {
            super::super::Frame {
                lines: frame.overlay.lines,
                model_lines: &[],
                ribbons: &[],
                model_composite: frame.overlay.model_composite,
            }
        } else {
            frame.overlay
        };
        self.model_renderer.draw(
            handle,
            &mut encoder,
            &self.background_view,
            &self.target_view,
            Size::new(f64::from(self.params.width), f64::from(self.params.height)),
            model_overlay,
            (foreground_enabled && frame.foreground).then_some(&self.foreground_view),
            frame.overlay_composite,
            frame.overlay_opacity,
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

fn select_heaviest(ui: &mut Ui) -> anyhow::Result<(String, usize)> {
    let visualizer = ui.benchmark_visualizer_mut();
    visualizer.load_models();
    let Models::Ready(models) = &mut visualizer.models else {
        anyhow::bail!("models did not load")
    };
    let mut largest = (String::new(), 0);
    for _ in 0..models.position().1 {
        let model = models.current();
        let count = model.wireframe(visualizer.crease_threshold).edges().len();
        if count > largest.1 {
            largest = (model.id.clone(), count);
        }
        models.navigate(Adjustment::Increase);
    }
    for _ in 0..models.position().1 {
        if models.current().id == largest.0 {
            break;
        }
        models.navigate(Adjustment::Increase);
    }
    visualizer.scene.show(
        models.current(),
        visualizer.crease_threshold,
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
    let workload_mode = WorkloadMode::from_env()?;
    let components = components()?;
    let base_config = base_config()?;
    let preview_frames: Vec<usize> = std::env::var("BENGA_PIPELINE_PREVIEW_FRAMES")
        .unwrap_or_default()
        .split(',')
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().parse())
        .collect::<Result<_, _>>()?;
    let mut cases = Vec::new();
    for (width, height) in extents()? {
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
            ui.set_presentation(true);
            ui.set_debug(false);
            ui.set_timeline(false);
            let mut view = View::fit(Size::new(f64::from(width), f64::from(height)));
            view.set_ui_scale(2.0);
            view.set_zoom(config.zoom);
            let mut pipeline = Pipeline::new(width, height)?;
            ui.set_plate_mask(Some(pipeline.plate_mask.image()));
            ui.audio_event(AudioEvent::Config { samplerate: 48_000 });
            let workload_metadata = if let Some(mode) = workload_mode.poi() {
                poi_workload::configure(&mut ui, mode)?
            } else {
                let (model, raw_edges) = select_heaviest(&mut ui)?;
                json!({"model_id":model,"raw_edges":raw_edges})
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
            let mut previews = Vec::new();
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
                let count = frame.overlay.ribbons.len() + frame.overlay.model_lines.len() * 2;
                // Diagnostic only: scan outside Ui.draw and the explicitly
                // summed frame cost. Exactly zero vertex alphas cannot tint
                // any pixel in either themed model blend mode.
                let transparent = if index >= cpu_warmup + warmup {
                    frame
                        .overlay
                        .ribbons
                        .iter()
                        .filter(|ribbon| {
                            ribbon.start_color[3] == 0.0
                                && ribbon.end_color[3] == 0.0
                                && ribbon.third_color[3] == 0.0
                        })
                        .count()
                } else {
                    0
                };
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
                    poi_events.push(event);
                    poi_frames.push(if workload_mode.poi().is_some() {
                        poi_workload::snapshot(&mut ui)
                    } else {
                        json!(null)
                    });
                    let measured_index = index - cpu_warmup - warmup;
                    if preview_frames.contains(&measured_index) {
                        let filename = format!(
                            "{width}x{height}-{}-{}-frame{measured_index:04}.png",
                            workload_mode.name(),
                            components.name()
                        );
                        pipeline.write_preview(&directory.join(&filename))?;
                        previews.push(json!({"measured_frame":measured_index,"path":filename}));
                    }
                }
            }
            eprintln!("{width}x{height} {} {}: full completion {:.3}/{:.3}ms median/p95; Ui.draw {:.3}ms; encode {:.3}ms; modelGPU {:.3}ms; background {rasterized}/{measured}",
                    workload_mode.name(),components.name(),summary(&full_ms)["median"].as_f64().unwrap(),summary(&full_ms)["p95"].as_f64().unwrap(),summary(&draw_ms)["median"].as_f64().unwrap(),summary(&encode_ms)["median"].as_f64().unwrap(),summary(&model_gpu_ms)["median"].as_f64().unwrap());
            cases.push(json!({
                    "width":width,"height":height,"ui_scale":2.0,"fps":fps,"interval_seconds":1.0/f64::from(fps),
                    "workload":workload_mode.name(),"workload_metadata":workload_metadata,"components":components.name(),
                    "render_settings":{
                        "model_motion":config.model_motion,"shutter_frames":config.shutter_frames,
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
                }));
        }
    }
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec_pretty(&json!({
        "cases":cases,"force_background_rebuild":force_background,"fps":fps,"workload":workload_mode.name(),
        "read_saved_config":std::env::var_os("BENGA_PIPELINE_CONFIG_PATH").is_some(),
            "method":"Actual Ui.draw and synthetic stereo capture with controlled normal motion or actual Showcase POI scripts. Retained background/raster-asset caches, Vello animated foreground/control passes, plate masks, themed line renderer, and final composition. Physical extents and UI scale are explicit. One frame completes before the next; no surface acquisition, display wait, native window chrome, OS compositor or VSync.",
            "component_controls":"no_model omits only model lines/ribbons at GPU composition. no_foreground omits GPU plate rendering, retained foreground asset preparation, foreground rasterization and composition. Both controls still build the complete Ui.draw CPU scene/model, isolating render contributions.",
            "timing_scope":"full_completion_ms adds audio/script, controls, Ui.draw, render encoding/submission and GPU completion wait. It is isolated completion, not achieved application FPS. Model timestamps cover final composition, potentially including dependencies on prior Vello writes. Query readback is excluded. Model setup occurs before timing.",
        }))?,
    )?;
    Ok(())
}
