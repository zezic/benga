//! THROWAWAY: static geometry + GPU projected history + two-interval ribbons.
//! This module is deliberately excluded from normal builds.

mod fixtures;
mod gpu;
mod live;

use std::{
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{ensure, Context};
use serde_json::{json, Value};
use vello::{kurbo::Size, util::RenderContext, wgpu};

use super::{Frame, ModelComposite, Renderer};
use fixtures::{ModelFixture, MotionCase, Output};
use gpu::{FrameParams, GpuRibbons};

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderPath {
    Current,
    Fast,
    Coverage,
}

impl RenderPath {
    fn is_gpu(self) -> bool {
        self != Self::Current
    }
    fn name(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Fast => "gpu",
            Self::Coverage => "coverage",
        }
    }
}

/// Run the optional native comparison or the reproducible offscreen experiment.
pub fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help") {
        println!(
            "GPU ribbon PROTOTYPE\n\
            Default: native comparison window.\n\
            --headless                 Run timed comparisons and save images/JSON\n\
            --model ID                 Model ID, single, or crossing (default: heaviest)\n\
            --extent WIDTHxHEIGHT,...  Default: 1920x1080,3840x2160\n\
            --motions NAME,...         Headless motion filter (includes along_edge_translation)\n\
            --samples N                Timestamp samples per method (default: 64)\n\
            --alpha N                  Headless model opacity (default: 0.04)\n\
            --output PATH              Default: target/gpu-ribbon-prototype\n\
            --reference-samples N      Optional full-model float reference (default: 0)\n\
            --animate N                Save N full-resolution comparison frames for reversal\n\
            --live-seconds N           Automatically close live smoke run after N seconds"
        );
        return Ok(());
    }
    let value = |flag: &str| -> Option<&str> {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .map(String::as_str)
    };
    let model = match value("--model") {
        Some("single") => ModelFixture::single_edge(),
        Some("crossing") => ModelFixture::crossing_edges(),
        id => fixtures::load_model(id)?,
    };
    if !args.iter().any(|arg| arg == "--headless") {
        return live::run(model, value("--live-seconds").map(str::parse).transpose()?);
    }
    let samples: usize = value("--samples").unwrap_or("64").parse()?;
    ensure!((1..=1024).contains(&samples), "samples must be 1..=1024");
    let alpha: f32 = value("--alpha").unwrap_or("0.04").parse()?;
    ensure!(
        alpha.is_finite() && (0.0..=1.0).contains(&alpha),
        "invalid alpha"
    );
    let references: usize = value("--reference-samples").unwrap_or("0").parse()?;
    ensure!(references <= 128, "reference samples must be at most 128");
    let animate: usize = value("--animate").unwrap_or("0").parse()?;
    ensure!(animate <= 240, "animation is bounded to 240 frames");
    let directory = PathBuf::from(value("--output").unwrap_or("target/gpu-ribbon-prototype"));
    fs::create_dir_all(&directory)?;
    let extents = value("--extent")
        .unwrap_or("1920x1080,3840x2160")
        .split(',')
        .map(|item| -> anyhow::Result<(u32, u32)> {
            let (a, b) = item
                .split_once('x')
                .context("extent must be WIDTHxHEIGHT")?;
            let (a, b) = (a.parse()?, b.parse()?);
            ensure!(
                (64..=8192).contains(&a) && (64..=8192).contains(&b),
                "invalid extent"
            );
            Ok((a, b))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let motions: Vec<_> = MotionCase::ALL
        .into_iter()
        .filter(|case| {
            value("--motions").is_none_or(|list| list.split(',').any(|name| name == case.name()))
        })
        .collect();
    ensure!(!motions.is_empty(), "motion filter matched no cases");
    let mut cases = Vec::new();
    let mut adapters = Vec::new();
    for (width, height) in extents {
        let mut experiment =
            Offscreen::new(&model, width, height, wgpu::TextureFormat::Bgra8Unorm)?;
        adapters.push(experiment.adapter());
        for &motion in &motions {
            experiment.seed(&model, motion, 0.0, alpha)?;
            let projection = experiment.check_projection(&model, motion, 0.0)?;
            let mut output = Output::default();
            let mut build = Vec::new();
            for index in 0..24 {
                let before = Instant::now();
                model.build_baseline(width, height, motion, 0.0, alpha, &mut output);
                if index >= 8 {
                    build.push(before.elapsed().as_secs_f64() * 1000.0);
                }
            }
            let mut variants = Vec::new();
            for prototype in [RenderPath::Current, RenderPath::Fast, RenderPath::Coverage] {
                for _ in 0..16 {
                    experiment.submit(
                        &output,
                        motion,
                        0.0,
                        alpha,
                        ModelComposite::Additive,
                        prototype,
                        false,
                    );
                }
                experiment.wait()?;
                let mut gpu = Vec::new();
                let mut compute = Vec::new();
                let mut render = Vec::new();
                let mut cpu = Vec::new();
                for _ in 0..samples {
                    cpu.push(experiment.submit(
                        &output,
                        motion,
                        0.0,
                        alpha,
                        ModelComposite::Additive,
                        prototype,
                        true,
                    ));
                    let stamps = experiment.timestamps(if prototype.is_gpu() { 6 } else { 2 })?;
                    let first = stamps.iter().copied().fold(f64::INFINITY, f64::min);
                    let last = stamps.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    gpu.push(last - first);
                    compute.push(if prototype.is_gpu() {
                        stamps[1] - stamps[0]
                    } else {
                        0.0
                    });
                    render.push(if prototype.is_gpu() {
                        stamps[5] - stamps[4]
                    } else {
                        stamps[1] - stamps[0]
                    });
                }
                let method = prototype.name();
                eprintln!("{width}x{height} {} {method}: GPU {:.3}/{:.3}ms median/p95; CPU upload/encode {:.3}ms",
                    motion.name(),percentile(&gpu,0.5),percentile(&gpu,0.95),percentile(&cpu,0.5));
                variants.push(json!({"method":method,"gpu_total_ms":summary(&gpu),"gpu_compute_ms":summary(&compute),
                    "gpu_ribbon_render_ms":summary(&render),"cpu_upload_encode_submit_ms":summary(&cpu),
                    "gpu_samples_ms":gpu,"cpu_samples_ms":cpu,
                    "cpu_generated_ribbon_bytes":if prototype.is_gpu() {0} else {output.ribbons.len()*80},
                    "triangle_upper_bound":match prototype {RenderPath::Current=>output.ribbons.len(),RenderPath::Fast=>model.edges.len()*6,RenderPath::Coverage=>model.edges.len()*4},
                }));
            }
            let mut previews = Vec::new();
            for (theme, composite) in [
                ("dark", ModelComposite::Additive),
                ("light", ModelComposite::Multiplicative),
            ] {
                experiment.background(composite)?;
                for prototype in [RenderPath::Current, RenderPath::Fast, RenderPath::Coverage] {
                    let method = prototype.name();
                    experiment.submit(&output, motion, 0.0, alpha, composite, prototype, false);
                    let pixels = experiment.pixels()?;
                    let name = format!("{width}x{height}-{}-{theme}-{method}.png", motion.name());
                    write_png(&directory.join(&name), width, height, &pixels)?;
                    previews.push(name);
                }
            }
            experiment.background(ModelComposite::Additive)?;
            cases.push(json!({"width":width,"height":height,"motion":motion.name(),"projection_check":projection,
                "cpu_baseline_projection_and_build_ms":summary(&build),"methods":variants,"previews":previews}));
        }
        if references > 0 {
            let mut float =
                Offscreen::new(&model, width, height, wgpu::TextureFormat::Rgba16Float)?;
            let required = (model.edges.len() * 2 * references).next_power_of_two() as u64 * 80;
            ensure!(required <= u64::from(float.context.devices[float.device_id].device.limits().max_storage_buffer_binding_size),
                "reference exceeds storage binding limit; reduce --reference-samples or use --model single/crossing");
            for &motion in &motions {
                float.seed(&model, motion, 0.0, alpha)?;
                let baseline = model.baseline(width, height, motion, 0.0, alpha);
                let reference = model.reference(width, height, motion, 0.0, alpha, references);
                for (name, geometry, prototype) in [
                    ("current", &baseline, RenderPath::Current),
                    ("gpu", &baseline, RenderPath::Fast),
                    ("coverage", &baseline, RenderPath::Coverage),
                    ("reference", &reference, RenderPath::Current),
                ] {
                    float.submit(
                        geometry,
                        motion,
                        0.0,
                        alpha,
                        ModelComposite::Additive,
                        prototype,
                        false,
                    );
                    write_png(
                        &directory.join(format!(
                            "{width}x{height}-{}-float-{name}.png",
                            motion.name()
                        )),
                        width,
                        height,
                        &float.pixels()?,
                    )?;
                }
            }
        }
        if animate > 0 {
            let frames = directory.join(format!("animation-{width}x{height}"));
            fs::create_dir_all(&frames)?;
            for index in 0..animate {
                // Display frames move by a tenth of an exposure interval to make
                // the reversal and curved history legible at playback speed.
                let frame = index as f32 * 0.1;
                experiment.seed(&model, MotionCase::Reverse, frame, alpha)?;
                let output = model.baseline(width, height, MotionCase::Reverse, frame, alpha);
                for prototype in [RenderPath::Current, RenderPath::Fast, RenderPath::Coverage] {
                    experiment.submit(
                        &output,
                        MotionCase::Reverse,
                        frame,
                        alpha,
                        ModelComposite::Additive,
                        prototype,
                        false,
                    );
                    write_png(
                        &frames.join(format!("{}-{index:04}.png", prototype.name())),
                        width,
                        height,
                        &experiment.pixels()?,
                    )?;
                }
            }
        }
    }
    let checks = sparse_checks(&directory)?;
    let report = json!({"prototype":true,"model":model.id,"vertices":model.vertices.len(),"edges":model.edges.len(),
        "shutter_frames":2,"history_samples":3,"alpha":alpha,"samples":samples,"adapters":adapters,"cases":cases,
        "methods":{"current":"Unchanged CPU endpoint sweep","gpu":"GPU vertex-colored body and two wings, actual middle pose","coverage":"GPU two-interval inverse bilinear coverage"},
        "gpu_parameter_upload_bytes_per_frame":std::mem::size_of::<FrameParams>(),
        "gpu_projected_history_bytes":model.vertices.len()*3*16,
        "sparse_checks":checks,"format":"Bgra8Unorm for timing and dark/light previews; Rgba16Float for temporal references",
        "timing_scope":"GPU total from compute-pass beginning through final render-pass end; current renderer single pass. GPU prototype includes compute, cached-background draw, ribbon draw and transitions. CPU upload/encode excludes baseline geometry build (reported separately). Single-frame samples wait before Metal timestamp resolve; waits/readback excluded from timestamps. No audio, UI, presentation, or sustained-FPS claim.",
        "quality_scope":"Prototype uses a normalized tent-shutter coverage model; production is a stylized swept-line effect. Brightness and sampled reference therefore differ intentionally. Full resolution; no render-target downscaling.",
    });
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    eprintln!("Saved prototype comparisons to {}", directory.display());
    Ok(())
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    values[((values.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
}
fn summary(values: &[f64]) -> Value {
    json!({"median":percentile(values,0.5),"p95":percentile(values,0.95),"min":percentile(values,0.0),"max":percentile(values,1.0)})
}

struct Offscreen {
    context: RenderContext,
    device_id: usize,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    _background: wgpu::Texture,
    background_view: wgpu::TextureView,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    renderer: Renderer,
    ribbons: GpuRibbons,
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
}

impl Offscreen {
    fn new(
        model: &ModelFixture,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> anyhow::Result<Self> {
        let mut context = RenderContext::new();
        let device_id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor {
                label: Some("GPU ribbon prototype"),
                required_features: wgpu::Features::TIMESTAMP_QUERY,
                ..Default::default()
            },
        ))?;
        let device = &context.devices[device_id].device;
        ensure!(
            device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
            "GPU timestamps unavailable"
        );
        let texture = |label, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
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
            "Prototype background",
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let background_view = background.create_view(&Default::default());
        let target = texture(
            "Prototype target",
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let target_view = target.create_view(&Default::default());
        let renderer = Renderer::new(device, format);
        let ribbons = GpuRibbons::new(device, format, &model.vertices, &model.edges);
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("Prototype pass timings"),
            ty: wgpu::QueryType::Timestamp,
            count: 6,
        });
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 48,
                usage,
                mapped_at_creation: false,
            })
        };
        let resolve = buffer(
            "Prototype timing resolve",
            wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        );
        let readback = buffer(
            "Prototype timing readback",
            wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        );
        let mut result = Self {
            context,
            device_id,
            width,
            height,
            format,
            _background: background,
            background_view,
            target,
            target_view,
            renderer,
            ribbons,
            queries,
            resolve,
            readback,
        };
        result.background(ModelComposite::Additive)?;
        Ok(result)
    }
    fn adapter(&self) -> String {
        format!(
            "{:?}; {}x{} {:?}",
            self.context.devices[self.device_id].adapter().get_info(),
            self.width,
            self.height,
            self.format
        )
    }
    fn wait(&self) -> anyhow::Result<()> {
        self.context.devices[self.device_id]
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(30)),
            })?;
        Ok(())
    }
    fn background(&mut self, composite: ModelComposite) -> anyhow::Result<()> {
        let handle = &self.context.devices[self.device_id];
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        let brightness = if composite == ModelComposite::Additive {
            0.02
        } else {
            0.96
        };
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Prototype background initialization"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.background_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: brightness,
                            g: brightness,
                            b: brightness,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        handle.queue.submit([encoder.finish()]);
        Ok(())
    }
    fn seed(
        &mut self,
        _model: &ModelFixture,
        motion: MotionCase,
        frame: f32,
        alpha: f32,
    ) -> anyhow::Result<()> {
        let handle = &self.context.devices[self.device_id];
        for slot in 0..3 {
            self.ribbons.update(
                &handle.queue,
                FrameParams::new(
                    self.width,
                    self.height,
                    motion.pose_at(frame - 2.0 + slot as f32, self.height),
                    slot,
                    alpha,
                ),
            );
            let mut encoder = handle.device.create_command_encoder(&Default::default());
            self.ribbons.encode_project(&mut encoder, None);
            handle.queue.submit([encoder.finish()]);
        }
        self.wait()
    }
    fn submit(
        &mut self,
        output: &Output,
        motion: MotionCase,
        frame: f32,
        alpha: f32,
        composite: ModelComposite,
        prototype: RenderPath,
        timed: bool,
    ) -> f64 {
        let before = Instant::now();
        let handle = &self.context.devices[self.device_id];
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        if prototype.is_gpu() {
            self.ribbons.update(
                &handle.queue,
                FrameParams::new(
                    self.width,
                    self.height,
                    motion.pose_at(frame, self.height),
                    2,
                    alpha,
                ),
            );
            self.ribbons.encode_project(
                &mut encoder,
                timed.then_some(wgpu::ComputePassTimestampWrites {
                    query_set: &self.queries,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }),
            );
        }
        if timed {
            self.renderer.benchmark_timestamps(
                &self.queries,
                if prototype.is_gpu() { 2 } else { 0 },
                if prototype.is_gpu() { 3 } else { 1 },
            );
        }
        let geometry = if prototype.is_gpu() {
            Frame {
                lines: &[],
                model_lines: &[],
                ribbons: &[],
                model_composite: composite,
                model_edge_capacity: 0,
            }
        } else {
            output.frame(composite)
        };
        self.renderer.draw(
            handle,
            &mut encoder,
            &self.background_view,
            &self.target_view,
            Size::new(self.width as f64, self.height as f64),
            geometry,
            None,
            None,
            1.0,
            None,
        );
        if prototype.is_gpu() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Prototype GPU ribbons"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                timestamp_writes: timed.then_some(wgpu::RenderPassTimestampWrites {
                    query_set: &self.queries,
                    beginning_of_pass_write_index: Some(4),
                    end_of_pass_write_index: Some(5),
                }),
                ..Default::default()
            });
            if prototype == RenderPath::Fast {
                self.ribbons.draw_fast(&mut pass, composite);
            } else {
                self.ribbons.draw(&mut pass, composite);
            }
        }
        handle.queue.submit([encoder.finish()]);
        before.elapsed().as_secs_f64() * 1000.0
    }
    fn timestamps(&self, count: u32) -> anyhow::Result<Vec<f64>> {
        self.wait()?; // Metal sampling must complete before encoding resolve.
        let handle = &self.context.devices[self.device_id];
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        encoder.resolve_query_set(&self.queries, 0..count, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, u64::from(count) * 8);
        handle.queue.submit([encoder.finish()]);
        let bytes = self.map(&self.readback, u64::from(count) * 8)?;
        let period = f64::from(handle.queue.get_timestamp_period()) / 1e6;
        let values: Vec<_> = bytes
            .chunks_exact(8)
            .map(|chunk| u64::from_ne_bytes(chunk.try_into().unwrap()))
            .collect();
        ensure!(
            values.iter().all(|&value| value > 0 && value < u64::MAX),
            "invalid GPU timestamp"
        );
        // Different passes can overlap in flight; only each pass's own
        // beginning/end pair must be ordered. Measure the enclosing span.
        ensure!(
            values.chunks_exact(2).all(|pair| pair[1] >= pair[0]),
            "invalid GPU pass timestamp pair: {values:?}"
        );
        let origin = *values.iter().min().unwrap();
        Ok(values
            .into_iter()
            .map(|value| (value - origin) as f64 * period)
            .collect())
    }
    fn map(&self, buffer: &wgpu::Buffer, size: u64) -> anyhow::Result<Vec<u8>> {
        let slice = buffer.slice(0..size);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.wait()?;
        rx.recv_timeout(Duration::from_secs(30))??;
        let bytes = slice.get_mapped_range()?.to_vec();
        buffer.unmap();
        Ok(bytes)
    }
    fn read_buffer(&self, source: &wgpu::Buffer, size: u64) -> anyhow::Result<Vec<u8>> {
        let handle = &self.context.devices[self.device_id];
        let buffer = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Prototype validation readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(source, 0, &buffer, 0, size);
        handle.queue.submit([encoder.finish()]);
        self.map(&buffer, size)
    }
    fn check_projection(
        &self,
        model: &ModelFixture,
        motion: MotionCase,
        frame: f32,
    ) -> anyhow::Result<Value> {
        let bytes = self.read_buffer(
            &self.ribbons.projected_history,
            (model.vertices.len() * 3 * 16) as u64,
        )?;
        let mut max_pixel_error = 0.0_f32;
        let mut max_opacity_error = 0.0_f32;
        for slot in 0..3 {
            let expected = model.project_vertices(
                self.width,
                self.height,
                motion.pose_at(frame - 2.0 + slot as f32, self.height),
            );
            for (index, expected) in expected.iter().enumerate() {
                let offset = (slot * model.vertices.len() + index) * 16;
                let actual: Vec<f32> = bytes[offset..offset + 16]
                    .chunks_exact(4)
                    .map(|value| f32::from_ne_bytes(value.try_into().unwrap()))
                    .collect();
                ensure!(
                    actual.iter().all(|value| value.is_finite()),
                    "nonfinite GPU projection"
                );
                max_pixel_error = max_pixel_error
                    .max((actual[0] - expected[0]).abs())
                    .max((actual[1] - expected[1]).abs());
                max_opacity_error = max_opacity_error.max((actual[2] - expected[2]).abs());
                ensure!(actual[3] == expected[3], "GPU/CPU visibility differs");
            }
        }
        ensure!(
            max_pixel_error < 0.1 && max_opacity_error < 0.001,
            "GPU projection mismatch: {max_pixel_error}px/{max_opacity_error} opacity"
        );
        Ok(
            json!({"max_pixel_error":max_pixel_error,"max_opacity_error":max_opacity_error,"vertices_checked":model.vertices.len()*3}),
        )
    }
    fn pixels(&self) -> anyhow::Result<Vec<u8>> {
        let handle = &self.context.devices[self.device_id];
        let pixel_bytes = if self.format == wgpu::TextureFormat::Rgba16Float {
            8
        } else {
            4
        };
        let row_bytes = self.width * pixel_bytes;
        let padded = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let size = u64::from(padded) * u64::from(self.height);
        let buffer = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Prototype PNG readback"),
            size,
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
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        handle.queue.submit([encoder.finish()]);
        let bytes = self.map(&buffer, size)?;
        let mut pixels = Vec::with_capacity((self.width * self.height * 4) as usize);
        for row in bytes.chunks_exact(padded as usize) {
            for pixel in row[..row_bytes as usize].chunks_exact(pixel_bytes as usize) {
                if pixel_bytes == 8 {
                    for value in pixel.chunks_exact(2) {
                        let component = half(u16::from_le_bytes(value.try_into().unwrap()));
                        ensure!(component.is_finite(), "nonfinite float render output");
                        pixels.push((component.clamp(0.0, 1.0) * 255.0).round() as u8);
                    }
                } else {
                    pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                }
            }
        }
        Ok(pixels)
    }
}

fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = (bits >> 10) & 31;
    let mantissa = f32::from(bits & 1023);
    sign * if exponent == 0 {
        mantissa * 2.0_f32.powi(-24)
    } else if exponent == 31 {
        f32::INFINITY
    } else {
        (1.0 + mantissa / 1024.0) * 2.0_f32.powi(i32::from(exponent) - 15)
    }
}
fn write_png(path: &Path, width: u32, height: u32, pixels: &[u8]) -> anyhow::Result<()> {
    let mut encoder = png::Encoder::new(BufWriter::new(fs::File::create(path)?), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(pixels)?;
    Ok(())
}

fn sparse_checks(directory: &Path) -> anyhow::Result<Vec<Value>> {
    let mut results = Vec::new();
    for model in [ModelFixture::single_edge(), ModelFixture::crossing_edges()] {
        let (width, height) = (960, 540);
        let mut renderer = Offscreen::new(&model, width, height, wgpu::TextureFormat::Rgba16Float)?;
        for motion in [
            MotionCase::Still,
            MotionCase::Translate,
            MotionCase::AlongEdge,
            MotionCase::Rotate90,
            MotionCase::Reverse,
        ] {
            let alpha = 0.8;
            renderer.seed(&model, motion, 0.0, alpha)?;
            let projection = renderer.check_projection(&model, motion, 0.0)?;
            let baseline = model.baseline(width, height, motion, 0.0, alpha);
            let reference = model.reference(width, height, motion, 0.0, alpha, 128);
            let mut images = Vec::new();
            for (name, output, prototype) in [
                ("current", &baseline, RenderPath::Current),
                ("gpu", &baseline, RenderPath::Fast),
                ("coverage", &baseline, RenderPath::Coverage),
                ("reference", &reference, RenderPath::Current),
            ] {
                renderer.submit(
                    output,
                    motion,
                    0.0,
                    alpha,
                    ModelComposite::Additive,
                    prototype,
                    false,
                );
                let pixels = renderer.pixels()?;
                write_png(
                    &directory.join(format!("{}-{}-{name}.png", model.id, motion.name())),
                    width,
                    height,
                    &pixels,
                )?;
                images.push(pixels);
            }
            results.push(
                json!({"model":model.id,"motion":motion.name(),"projection":projection,
                "current_vs_reference":fixtures::image_metrics(&images[0],&images[3]),
                "gpu_vs_reference":fixtures::image_metrics(&images[1],&images[3]),
                "coverage_vs_reference":fixtures::image_metrics(&images[2],&images[3])}),
            );
        }
    }
    Ok(results)
}
