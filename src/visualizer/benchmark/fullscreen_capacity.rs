//! Hardware-oriented full-screen ribbon capacity, using production float shaders.
//! This freezes prepared geometry after three real projected poses. It measures
//! drawing, retained-background composition and final resolve, not animation,
//! projection, preparation, Vello UI, surface acquisition or the OS compositor.
//! BENGA_CAPACITY_FLAT_CONTROL=1 with FIXTURES=sweep replaces only model drawing
//! with a minimal flat shader as a separate fill/blend ceiling reference.
use std::{
    fmt::Write,
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{ensure, Context};
use serde_json::{json, Value};
use vello::{kurbo::Size, util::RenderContext, wgpu};

use super::{fullscreen_fixtures::RibbonFixture, summary};
use crate::visualizer::{gpu_bridge::GpuModelFrame, Frame, ModelComposite, Renderer};

const TIMEOUT: Duration = Duration::from_secs(30);
const FRAME_MS: f64 = 1000.0 / 60.0;
const PRACTICAL_GPU_MS: f64 = 12.0;
const MAX_BATCH: usize = 8;

struct Options {
    extents: Vec<[u32; 2]>,
    fixtures: Vec<RibbonFixture>,
    blends: Vec<ModelComposite>,
    counts: Vec<usize>,
    explicit_counts: bool,
    warmup: usize,
    samples: usize,
    batch: usize,
    paced_frames: usize,
    flat_control: bool,
    previews: bool,
    output: PathBuf,
}

impl Options {
    fn from_env() -> anyhow::Result<Self> {
        fn value(name: &str, fallback: &str) -> String {
            std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
        }
        fn integer(
            name: &str,
            fallback: &str,
            range: std::ops::RangeInclusive<usize>,
        ) -> anyhow::Result<usize> {
            let result = value(name, fallback)
                .parse()
                .with_context(|| format!("invalid {name}"))?;
            ensure!(range.contains(&result), "{name} outside {range:?}");
            Ok(result)
        }
        let extents = value("BENGA_CAPACITY_EXTENTS", "5120x2880")
            .split(',')
            .map(|extent| {
                let (w, h) = extent
                    .trim()
                    .split_once('x')
                    .context("capacity extent must be WIDTHxHEIGHT")?;
                let result = [w.parse::<u32>()?, h.parse::<u32>()?];
                ensure!(
                    result.iter().all(|v| *v > 0),
                    "capacity extent must be nonzero"
                );
                Ok(result)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let fixtures = value("BENGA_CAPACITY_FIXTURES", "sweep,reversal,crossing")
            .split(',')
            .map(|name| {
                RibbonFixture::parse(name.trim())
                    .with_context(|| format!("unknown capacity fixture {name}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let blends = value("BENGA_CAPACITY_BLENDS", "additive,multiply")
            .split(',')
            .map(|name| match name.trim() {
                "additive" => Ok(ModelComposite::Additive),
                "multiply" | "multiplicative" => Ok(ModelComposite::Multiplicative),
                other => anyhow::bail!("unknown capacity blend {other}"),
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let flat_control = match value("BENGA_CAPACITY_FLAT_CONTROL", "0").as_str() {
            "0" => false,
            "1" => true,
            _ => anyhow::bail!("BENGA_CAPACITY_FLAT_CONTROL must be 0 or 1"),
        };
        ensure!(
            !flat_control
                || fixtures
                    .iter()
                    .all(|fixture| *fixture == RibbonFixture::Sweep),
            "flat fill control requires BENGA_CAPACITY_FIXTURES=sweep"
        );
        let explicit_counts = std::env::var_os("BENGA_CAPACITY_COUNTS").is_some();
        let mut counts = value(
            "BENGA_CAPACITY_COUNTS",
            "0,1,2,4,8,16,32,64,128,256,512,1024",
        )
        .split(',')
        .map(|n| n.trim().parse::<usize>().context("invalid capacity count"))
        .collect::<anyhow::Result<Vec<_>>>()?;
        ensure!(
            !counts.is_empty() && counts.iter().all(|n| *n <= 4096),
            "capacity counts must be 0..=4096"
        );
        counts.sort_unstable();
        counts.dedup();
        let paced_seconds = value("BENGA_CAPACITY_PACED_SECONDS", "0").parse::<f64>()?;
        ensure!(
            paced_seconds.is_finite()
                && (paced_seconds == 0.0 || (0.5..=10.0).contains(&paced_seconds)),
            "paced seconds must be 0/off or 0.5..=10"
        );
        let paced_frames = match std::env::var("BENGA_CAPACITY_PACED_FRAMES") {
            Ok(value) => {
                let frames = value.parse::<usize>()?;
                ensure!(
                    frames == 0 || (120..=600).contains(&frames),
                    "paced frames must be 0/off or 120..=600"
                );
                frames
            }
            Err(std::env::VarError::NotPresent) => (paced_seconds * 60.0).round() as usize,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            extents,
            fixtures,
            blends,
            counts,
            explicit_counts,
            warmup: integer("BENGA_CAPACITY_WARMUP", "8", 1..=512)?,
            samples: integer("BENGA_CAPACITY_SAMPLES", "32", 1..=2048)?,
            batch: integer("BENGA_CAPACITY_BATCH", "1", 1..=MAX_BATCH)?,
            paced_frames,
            flat_control,
            previews: value("BENGA_CAPACITY_PREVIEWS", "0") == "1",
            output: PathBuf::from(value("BENGA_CAPACITY_OUTPUT", "target/ribbon-capacity")),
        })
    }
}

#[derive(Default)]
struct Timings {
    composition_ms: Vec<f64>,
    resolve_ms: Vec<f64>,
    inter_pass_gap_ms: Vec<f64>,
    render_span_ms: Vec<f64>,
    batch_span_per_frame_ms: Vec<f64>,
    cpu_submit_ms: Vec<f64>,
    completion_wait_ms: Vec<f64>,
    host_batch_complete_per_frame_ms: Vec<f64>,
    query_readback_ms: Vec<f64>,
}

struct CapacityBench {
    context: RenderContext,
    device_id: usize,
    renderer: Renderer,
    _background: wgpu::Texture,
    background_view: wgpu::TextureView,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    queries: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    readback: wgpu::Buffer,
    extent: [u32; 2],
    period_ns: f64,
    flat_pipelines: Option<[wgpu::RenderPipeline; 2]>,
}

impl CapacityBench {
    fn new(extent: [u32; 2], flat_control: bool) -> anyhow::Result<Self> {
        let mut context = RenderContext::new();
        let device_id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor {
                required_features: wgpu::Features::TIMESTAMP_QUERY,
                ..Default::default()
            },
        ))?;
        let device = &context.devices[device_id];
        ensure!(
            device
                .device
                .features()
                .contains(wgpu::Features::TIMESTAMP_QUERY),
            "GPU timestamp queries unavailable"
        );
        ensure!(
            Renderer::gpu_ribbons_supported(&device.device)
                && Renderer::gpu_ribbons_adapter_supported(device.adapter()),
            "production GPU ribbons unsupported"
        );
        ensure!(
            extent
                .iter()
                .all(|n| *n <= device.device.limits().max_texture_dimension_2d),
            "extent exceeds device limits"
        );
        let texture = |label, usage| {
            device.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: extent[0],
                    height: extent[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage,
                view_formats: &[],
            })
        };
        let background = texture(
            "Capacity retained background",
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let target = texture(
            "Capacity resolved surface",
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let background_view = background.create_view(&Default::default());
        let target_view = target.create_view(&Default::default());
        let queries = device.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("Capacity float composition and resolve timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: (MAX_BATCH * 4) as u32,
        });
        let bytes = (MAX_BATCH * 4 * 8) as u64;
        let resolved = device.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Capacity timestamp resolve"),
            size: bytes,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Capacity timestamp readback"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let renderer = Renderer::new(&device.device, wgpu::TextureFormat::Bgra8Unorm);
        let period_ns = f64::from(device.queue.get_timestamp_period());
        ensure!(
            period_ns.is_finite() && period_ns > 0.0,
            "invalid GPU timestamp period"
        );
        let flat_pipelines = flat_control.then(|| flat_fill_pipelines(&device.device));
        Ok(Self {
            context,
            device_id,
            renderer,
            _background: background,
            background_view,
            target,
            target_view,
            queries,
            resolved,
            readback,
            extent,
            period_ns,
            flat_pipelines,
        })
    }

    fn wait(&self) -> anyhow::Result<()> {
        self.context.devices[self.device_id]
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(TIMEOUT),
            })?;
        Ok(())
    }

    fn adapter(&self) -> Value {
        let handle = &self.context.devices[self.device_id];
        let info = handle.adapter().get_info();
        json!({"name":info.name,"backend":format!("{:?}",info.backend),
            "device_type":format!("{:?}",info.device_type),"driver":info.driver,
            "driver_info":info.driver_info,"timestamp_period_ns":self.period_ns})
    }

    fn coverage(&self) -> anyhow::Result<Value> {
        self.renderer
            .benchmark_coverage(&self.context.devices[self.device_id])
    }

    fn draw(&mut self, model: &GpuModelFrame, composite: ModelComposite, query: Option<u32>) {
        let handle = &self.context.devices[self.device_id];
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        if let Some(begin) = query {
            self.renderer
                .benchmark_timestamps(&self.queries, begin, begin + 1);
            self.renderer
                .benchmark_resolve_timestamps(&self.queries, begin + 2, begin + 3);
        }
        self.renderer.draw(
            handle,
            &mut encoder,
            &self.background_view,
            &self.target_view,
            Size::new(f64::from(self.extent[0]), f64::from(self.extent[1])),
            Frame {
                lines: &[],
                model_composite: composite,
                gpu_models: std::slice::from_ref(model),
            },
            None,
            None,
            1.0,
            None,
            None,
        );
        handle.queue.submit([encoder.finish()]);
    }

    fn seed(
        &mut self,
        fixture: RibbonFixture,
        model: &mut GpuModelFrame,
        composite: ModelComposite,
    ) -> anyhow::Result<()> {
        self.renderer.benchmark_fill_override(None);
        self.renderer.benchmark_freeze_gpu(false);
        let handle = &self.context.devices[self.device_id];
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        let level = if composite == ModelComposite::Additive {
            0.02
        } else {
            0.95
        };
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Initialize capacity retained background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.background_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: level,
                            g: level,
                            b: level,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        handle.queue.submit([encoder.finish()]);
        for pose in 0..3 {
            fixture.set_pose(model, self.extent, pose);
            self.draw(model, composite, None);
            self.wait()?;
        }
        self.renderer.benchmark_freeze_gpu(true);
        if let Some(pipelines) = &self.flat_pipelines {
            let index = if composite == ModelComposite::Additive {
                0
            } else {
                1
            };
            self.renderer.benchmark_fill_override(Some((
                pipelines[index].clone(),
                model.topology.edges.len() as u32,
            )));
        }
        Ok(())
    }

    fn warmup(
        &mut self,
        model: &GpuModelFrame,
        composite: ModelComposite,
        frames: usize,
    ) -> anyhow::Result<()> {
        for _ in 0..frames {
            self.draw(model, composite, None);
            self.wait()?;
        }
        Ok(())
    }

    fn measure(
        &mut self,
        model: &GpuModelFrame,
        composite: ModelComposite,
        samples: usize,
        batch: usize,
    ) -> anyhow::Result<Timings> {
        let mut timing = Timings::default();
        while timing.render_span_ms.len() < samples {
            let count = batch.min(samples - timing.render_span_ms.len());
            let batch_start = Instant::now();
            for i in 0..count {
                let submit = Instant::now();
                self.draw(model, composite, Some((i * 4) as u32));
                timing
                    .cpu_submit_ms
                    .push(submit.elapsed().as_secs_f64() * 1000.0);
            }
            let wait = Instant::now();
            self.wait()?;
            timing
                .completion_wait_ms
                .push(wait.elapsed().as_secs_f64() * 1000.0 / count as f64);
            timing
                .host_batch_complete_per_frame_ms
                .push(batch_start.elapsed().as_secs_f64() * 1000.0 / count as f64);

            // Resolve after host-observed completion, matching the existing
            // Metal benchmark workaround. Diagnostic readback is untimed.
            let diagnostic = Instant::now();
            let handle = &self.context.devices[self.device_id];
            let mut encoder = handle.device.create_command_encoder(&Default::default());
            let words = count * 4;
            let bytes = (words * 8) as u64;
            encoder.resolve_query_set(&self.queries, 0..words as u32, &self.resolved, 0);
            encoder.copy_buffer_to_buffer(&self.resolved, 0, &self.readback, 0, bytes);
            handle.queue.submit([encoder.finish()]);
            let (tx, rx) = mpsc::channel();
            let slice = self.readback.slice(0..bytes);
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            self.wait()?;
            rx.recv_timeout(TIMEOUT)
                .context("capacity timestamp callback timed out")??;
            {
                let mapped = slice.get_mapped_range()?;
                let mut first = u64::MAX;
                let mut last = 0;
                for (i, record) in mapped.chunks_exact(32).enumerate() {
                    let mut t = [0; 4];
                    for (dst, bytes) in t.iter_mut().zip(record.chunks_exact(8)) {
                        *dst = u64::from_ne_bytes(bytes.try_into().unwrap());
                    }
                    ensure!(
                        t.iter().all(|n| *n != 0 && *n != u64::MAX)
                            && t[1] >= t[0]
                            && t[3] >= t[2]
                            && t[3] >= t[1]
                            && t[3] >= t[0],
                        "invalid capacity GPU timestamps at sample {i}: {t:?}"
                    );
                    let ms = |ticks| ticks as f64 * self.period_ns / 1e6;
                    timing.composition_ms.push(ms(t[1] - t[0]));
                    timing.resolve_ms.push(ms(t[3] - t[2]));
                    // Vertex stages may overlap an earlier pass on Metal.
                    // Preserve the signed boundary gap instead of adding
                    // individual pass durations and double-counting overlap.
                    timing
                        .inter_pass_gap_ms
                        .push((t[2] as f64 - t[1] as f64) * self.period_ns / 1e6);
                    timing.render_span_ms.push(ms(t[3] - t[0]));
                    first = first.min(t[0]);
                    last = last.max(t[3]);
                }
                timing
                    .batch_span_per_frame_ms
                    .push((last - first) as f64 * self.period_ns / 1e6 / count as f64);
            }
            self.readback.unmap();
            timing
                .query_readback_ms
                .push(diagnostic.elapsed().as_secs_f64() * 1000.0);
        }
        Ok(timing)
    }

    /// A bounded60Hz submission replay. Completion callbacks are polled every
    /// 0.5ms, so callback latency is a conservative host observation, not a GPU
    /// execution timestamp. Limit3frames in flight to expose missed capacity.
    fn paced(
        &mut self,
        model: &GpuModelFrame,
        composite: ModelComposite,
        frames: usize,
    ) -> anyhow::Result<Value> {
        let seconds = frames as f64 / 60.0;
        let (tx, rx) = mpsc::channel::<(Instant, Instant)>();
        let mut submitted = 0;
        let mut completed = 0;
        let mut skipped = 0;
        let mut pending = 0;
        let mut max_pending = 0;
        let mut submit_lateness_ms = Vec::new();
        let mut latencies_ms = Vec::new();
        let mut completion_at = Vec::new();
        let start = Instant::now();
        for frame in 0..frames {
            let deadline = start + Duration::from_secs_f64(frame as f64 / 60.0);
            loop {
                self.context.devices[self.device_id]
                    .device
                    .poll(wgpu::PollType::Poll)?;
                while let Ok((due, at)) = rx.try_recv() {
                    completed += 1;
                    pending -= 1;
                    latencies_ms.push(at.duration_since(due).as_secs_f64() * 1000.0);
                    completion_at.push(at);
                }
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(
                    Duration::from_micros(500)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            if pending >= 3 {
                skipped += 1;
                continue;
            }
            submit_lateness_ms.push(
                Instant::now()
                    .saturating_duration_since(deadline)
                    .as_secs_f64()
                    * 1000.0,
            );
            self.draw(model, composite, None);
            let tx = tx.clone();
            self.context.devices[self.device_id]
                .queue
                .on_submitted_work_done(move || {
                    let _ = tx.send((deadline, Instant::now()));
                });
            submitted += 1;
            pending += 1;
            max_pending = max_pending.max(pending);
        }
        let timeout = Instant::now() + TIMEOUT;
        while completed < submitted {
            ensure!(Instant::now() < timeout, "paced capacity replay timed out");
            self.context.devices[self.device_id]
                .device
                .poll(wgpu::PollType::Poll)?;
            while let Ok((due, at)) = rx.try_recv() {
                completed += 1;
                latencies_ms.push(at.duration_since(due).as_secs_f64() * 1000.0);
                completion_at.push(at);
            }
            if completed < submitted {
                thread::sleep(Duration::from_micros(500));
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        let completion_fps = if completion_at.len() > 1 {
            (completion_at.len() - 1) as f64
                / completion_at
                    .last()
                    .unwrap()
                    .duration_since(completion_at[0])
                    .as_secs_f64()
        } else {
            0.0
        };
        let latency = summary(&latencies_ms);
        let completion_deadline_misses = latencies_ms.iter().filter(|ms| **ms > FRAME_MS).count();
        let submission_deadline_misses = submit_lateness_ms
            .iter()
            .filter(|ms| **ms >= FRAME_MS)
            .count();
        Ok(
            json!({"target_fps":60,"requested_seconds":seconds,"elapsed_seconds":elapsed,
            "scheduled_frames":frames,"submitted_frames":submitted,"completed_frames":completed,
            "skipped_due_to_three_in_flight":skipped,"max_frames_in_flight":max_pending,
            "completion_cadence_fps":completion_fps,"scheduled_to_completion_ms":latency,
            "submit_lateness_ms":summary(&submit_lateness_ms),
            "host_observed_completion_deadline_misses":completion_deadline_misses,
            "submission_lateness_over_one_frame":submission_deadline_misses,
            "all_scheduled_frames_submitted":skipped==0,
            "meets_60hz_p95_completion_budget":skipped==0 && latency["p95"].as_f64().unwrap()<=FRAME_MS,
            "all_host_observed_completion_deadlines_met":skipped==0 && completion_deadline_misses==0,
            "scope":"Paced offscreen production draws; no display/OS compositor. Host completion callbacks polled at up to0.5ms cadence; latency includes poll/scheduling delay. No query readback during replay."}),
        )
    }

    fn write_preview(&self, path: &Path) -> anyhow::Result<()> {
        let handle = &self.context.devices[self.device_id];
        let [width, height] = self.extent;
        let row = (width * 4).div_ceil(256) * 256;
        let buffer = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Capacity preview readback"),
            size: u64::from(row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            self.target.size(),
        );
        handle.queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        self.wait()?;
        rx.recv_timeout(TIMEOUT)??;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        {
            let mapped = buffer.slice(..).get_mapped_range()?;
            for row in mapped.chunks_exact(row as usize) {
                pixels.extend_from_slice(&row[..width as usize * 4]);
            }
        }
        buffer.unmap();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let mut png = png::Encoder::new(BufWriter::new(File::create(path)?), width, height);
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.write_header()?.write_image_data(&pixels)?;
        Ok(())
    }
}

fn blend_name(blend: ModelComposite) -> &'static str {
    match blend {
        ModelComposite::Additive => "additive",
        ModelComposite::Multiplicative => "multiply",
    }
}

/// Deliberately tiny shader used only to locate a blend/fill ceiling. All
/// per-layer color work runs in the vertex stage; the fragment stage returns
/// a flat, nonzero, instance-dependent color. It is not a ribbon shader.
fn flat_fill_pipelines(device: &wgpu::Device) -> [wgpu::RenderPipeline; 2] {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Capacity minimal flat fill control"),
        source: wgpu::ShaderSource::Wgsl(r#"
struct V {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) ink: vec4<f32>,
}
fn vertex_data(corner: u32, instance: u32, multiply: bool) -> V {
    let positions = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let hash = instance * 1664525u + 1013904223u;
    let variation = f32(hash & 255u) / 255.0;
    let rgb = vec3(0.2 + variation * 0.1, 0.6 + variation * 0.2, 0.9);
    let alpha = 0.01 * (0.8 + variation * 0.2);
    var ink = vec4(rgb * alpha, alpha);
    if multiply { ink = vec4(mix(vec3(1.0), rgb, alpha), 1.0); }
    return V(vec4(positions[corner], 0.0, 1.0), ink);
}
@vertex fn additive_vertex(@builtin(vertex_index) corner: u32, @builtin(instance_index) instance: u32) -> V {
    return vertex_data(corner, instance, false);
}
@vertex fn multiply_vertex(@builtin(vertex_index) corner: u32, @builtin(instance_index) instance: u32) -> V {
    return vertex_data(corner, instance, true);
}
@fragment fn flat_fragment(value: V) -> @location(0) vec4<f32> { return value.ink; }
"#.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Capacity resource-free flat fill layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    [false, true].map(|multiply| {
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
            label: Some(if multiply {
                "Capacity flat multiply"
            } else {
                "Capacity flat additive"
            }),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(if multiply {
                    "multiply_vertex"
                } else {
                    "additive_vertex"
                }),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("flat_fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
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
    })
}

#[test]
#[ignore = "requires a timestamp-capable GPU; writes hardware capacity samples"]
fn measures_fullscreen_ribbon_capacity() -> anyhow::Result<()> {
    let options = Options::from_env()?;
    fs::create_dir_all(&options.output)?;
    let mut cases = Vec::new();
    let mut capacities = Vec::new();
    let mut adapters = Vec::new();
    let mut csv = String::from("width,height,fixture,blend,ribbons,nominal_screen_layers,batch,gpu_render_median_ms,gpu_render_p95_ms,gpu_composition_median_ms,gpu_resolve_pass_interval_median_ms,gpu_batch_per_frame_median_ms,host_complete_per_frame_median_ms\n");
    for &extent in &options.extents {
        let mut bench = CapacityBench::new(extent, options.flat_control)?;
        adapters.push(bench.adapter());
        for &fixture in &options.fixtures {
            let fixture_label = if options.flat_control {
                "flat-control"
            } else {
                fixture.name()
            };
            for &composite in &options.blends {
                let mut passing_frame = None;
                let mut first_failing_frame = None;
                let mut passing_practical = None;
                for &count in &options.counts {
                    let mut model = fixture.model(extent[0], extent[1], count);
                    bench.seed(fixture, &mut model, composite)?;
                    bench.warmup(&model, composite, options.warmup)?;
                    let timing =
                        bench.measure(&model, composite, options.samples, options.batch)?;
                    let full = summary(&timing.render_span_ms);
                    let composition = summary(&timing.composition_ms);
                    let resolve = summary(&timing.resolve_ms);
                    let batch = summary(&timing.batch_span_per_frame_ms);
                    let host = summary(&timing.host_batch_complete_per_frame_ms);
                    let p95 = full["p95"].as_f64().unwrap();
                    let budget_p95 = if options.batch == 1 {
                        p95
                    } else {
                        batch["p95"].as_f64().unwrap()
                    };
                    if budget_p95 <= FRAME_MS {
                        passing_frame = Some(count);
                    } else if first_failing_frame.is_none() {
                        first_failing_frame = Some(count);
                    }
                    if budget_p95 <= PRACTICAL_GPU_MS {
                        passing_practical = Some(count);
                    }
                    let paced = if options.paced_frames > 0 {
                        Some(bench.paced(&model, composite, options.paced_frames)?)
                    } else {
                        None
                    };
                    eprintln!("{}x{} {} {} repetitions={count}: render {:.3}/{p95:.3}ms median/p95, composition {:.3}ms, resolve interval {:.3}ms, batch {:.3}ms/frame",
                        extent[0],extent[1],fixture_label,blend_name(composite),full["median"].as_f64().unwrap(),
                        composition["median"].as_f64().unwrap(),resolve["median"].as_f64().unwrap(),batch["median"].as_f64().unwrap());
                    writeln!(
                        csv,
                        "{},{},{},{},{},{},{},{},{},{},{},{},{}",
                        extent[0],
                        extent[1],
                        fixture_label,
                        blend_name(composite),
                        count,
                        count as f64 * fixture.nominal_coverage_screens_per_ribbon(),
                        options.batch,
                        full["median"],
                        full["p95"],
                        composition["median"],
                        resolve["median"],
                        batch["median"],
                        host["median"]
                    )?;
                    let mut case = json!({"width":extent[0],"height":extent[1],"fixture":fixture_label,
                        "fixture_description":if options.flat_control {"Resource-free flat fragment shader, fullscreen triangle per instance; artificial blend/fill ceiling, not ribbons."} else {fixture.description()},"blend":blend_name(composite),"ribbons":count,
                        "nominal_screen_layers":count as f64*fixture.nominal_coverage_screens_per_ribbon(),
                        "nominal_screens_per_ribbon":fixture.nominal_coverage_screens_per_ribbon(),
                        "warmup_frames":options.warmup,"samples":options.samples,"batch_size":options.batch,
                        "passes_16_667ms_gpu_p95":budget_p95<=FRAME_MS,"passes_12ms_gpu_p95":budget_p95<=PRACTICAL_GPU_MS,
                        "gpu_budget_metric":if options.batch==1 {"gpu_render_span_ms"} else {"gpu_batch_span_per_frame_ms"}});
                    case["workload_kind"] = json!(if options.flat_control {
                        "minimal_flat_fill_control"
                    } else {
                        "production_ribbons"
                    });
                    case["repetition_unit"] = json!(if options.flat_control {
                        "full-screen flat layers; ribbons field is the common layer-count axis"
                    } else {
                        "logical source-edge ribbons"
                    });
                    if options.flat_control {
                        case["flat_control_settings"] = json!({"vertices_per_layer":3,
                            "fullscreen_layers":count,"fragment_stage":"returns flat interpolated ink unchanged",
                            "alpha_range":[0.008,0.01],"RGB_ranges":[[0.2,0.3],[0.6,0.8],[0.9,0.9]],
                            "instance_variation":"deterministic u32 LCG hash, low8bits/255",
                            "seeded_ribbon_settings_below_are_unused_in_timed_model_draw":true});
                    }
                    let mut poses = Vec::with_capacity(3);
                    for pose in 0..3 {
                        fixture.set_pose(&mut model, extent, pose);
                        poses.push(model.projected.clone());
                    }
                    case["fixture_settings"] = json!({"shared_vertices":model.topology.vertex_count,
                        "source_edges":model.topology.edges.len(),"projected_history_poses":poses,
                        "stroke_width_pixels":model.width,"sample_edge_ink_rgba":model.styles.first(),
                        "motion_fade_power":model.motion_fade_power,"motion_opacity_floor":model.motion_opacity,
                        "interval_seconds":model.durations,"prepared_records_frozen":true});
                    case["timings"] = json!({"gpu_render_span_ms":full,"gpu_composition_ms":composition,"gpu_resolve_ms":resolve,
                        "gpu_inter_pass_gap_ms":summary(&timing.inter_pass_gap_ms),
                        "gpu_batch_span_per_frame_ms":batch,"cpu_encode_submit_ms":summary(&timing.cpu_submit_ms),
                        "host_completion_wait_ms_per_frame":summary(&timing.completion_wait_ms),
                        "host_batch_complete_ms_per_frame":host,"query_readback_ms_per_batch":summary(&timing.query_readback_ms)});
                    case["raw_samples"] = json!({"gpu_render_span_ms":timing.render_span_ms,"gpu_composition_ms":timing.composition_ms,
                        "gpu_inter_pass_gap_ms":timing.inter_pass_gap_ms,
                        "gpu_resolve_ms":timing.resolve_ms,"gpu_batch_span_per_frame_ms":timing.batch_span_per_frame_ms,
                        "cpu_encode_submit_ms":timing.cpu_submit_ms,"host_completion_wait_ms_per_frame":timing.completion_wait_ms,
                        "host_batch_complete_ms_per_frame":timing.host_batch_complete_per_frame_ms,"query_readback_ms_per_batch":timing.query_readback_ms});
                    case["paced"] = json!(paced);
                    if options.previews && count == 1 {
                        let name = format!(
                            "{}x{}-{}-{}-N1.png",
                            extent[0],
                            extent[1],
                            fixture_label,
                            blend_name(composite)
                        );
                        bench.write_preview(&options.output.join(&name))?;
                        case["preview"] = json!(name);
                        if !options.flat_control {
                            // Validate the nominal fixture area from the real
                            // prepared GPU records, outside all frame timings.
                            case["geometric_coverage"] = bench.coverage()?;
                        }
                    }
                    cases.push(case);
                    // Preserve completed samples even if a later count hits a
                    // device timeout. This I/O runs outside every measured
                    // frame and the next case has its own warmup.
                    fs::write(
                        options.output.join("partial-results.json"),
                        serde_json::to_vec_pretty(&json!({
                        "complete":false,"adapters":adapters,"cases":cases,"capacity_bounds":capacities,
                        "scope":"Checkpoint of completed cases; use results.json for final complete metadata."}))?,
                    )?;
                    if !options.explicit_counts && budget_p95 > FRAME_MS * 2.0 {
                        break;
                    }
                }
                capacities.push(json!({"width":extent[0],"height":extent[1],"fixture":fixture_label,"blend":blend_name(composite),
                    "gpu_budget_metric":if options.batch==1 {"gpu_render_span_ms"} else {"gpu_batch_span_per_frame_ms"},
                    "largest_tested_count_under_16_667ms_p95":passing_frame,"first_tested_count_over_16_667ms_p95":first_failing_frame,
                    "largest_tested_count_under_12ms_p95":passing_practical,
                    "scope":"Bounds from tested counts, not an interpolated exact threshold.12ms is a conservative render allowance, not a measured full-application guarantee."}));
            }
        }
    }
    let results = json!({"adapters":adapters,"cases":cases,"capacity_bounds":capacities,
        "complete":true,
        "flat_fill_control":options.flat_control,
        "gpu_frame_budget_ms":FRAME_MS,"practical_gpu_allowance_ms":PRACTICAL_GPU_MS,
        "format":"Rgba16Float model/background accumulation, Bgra8Unorm final resolve",
        "geometry_scope":if options.flat_control {"Production setup seeds3poses outside timing, then the model draw is replaced by one full-screen triangle per flat layer. The fragment shader only returns a flat nonzero instance-dependent color; no AA, history, mask, depth, coverage or resource reads. Not a ribbon or asset budget."} else {"Production shared-history ribbon preparation seeds3poses once per case, then freezes prepared records. Repeated rendering keeps all source edges, shader classification, style/depth/motion shading and deterministic draw order."},
        "timing_scope":"GPU render span starts at float composition pass and ends at final resolve end. It includes retained-background draw, ribbon drawing, float-target store and final resolve. CPU projection/preparation/upload and Vello UI/audio/surface/compositor are excluded. Timestamp resolution/readback occurs after host-observed completion and is excluded from frame-completion metrics.",
        "batch_scope":"Batch1 gives isolated GPU pass latency. In larger queued batches, individual pass spans may include pipeline overlap/dependency latency; use GPU batch span/frame and host batch completion/frame for throughput.",
        "resolve_interval_scope":"gpu_resolve_ms is the resolve pass begin/end interval, not isolated resolve work: its vertex-start timestamp can precede completion of the upstream float pass and include dependency waiting. Use gpu_render_span_ms(q3-q0) for total GPU latency; do not add composition and resolve intervals.",
        "asset_scope":if options.flat_control {"Artificial minimal shader/blend/fill ceiling. A flat layer is one full-viewport triangle instance. It is not a ribbon or asset capacity; production coverage, AA, masks and varying gradients cost additional work."} else {"A screen layer means rasterized coverage equal to the whole viewport, counting overlap repeatedly, not union coverage or model edge count. Fixture nominal coverage is identified separately. Asset budgets need headroom for preparation/UI and expensive crossing shaders."}});
    fs::write(
        options.output.join("results.json"),
        serde_json::to_vec_pretty(&results)?,
    )?;
    fs::write(options.output.join("summary.csv"), csv)?;
    Ok(())
}
