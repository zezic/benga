//! Offscreen measurements through the real line renderer, without display pacing.
use std::{
    fs::File,
    io::BufWriter,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{ensure, Context};
use vello::{kurbo::Size, util::RenderContext, wgpu};

use super::super::{Frame, Renderer};

const BATCH: usize = 8;
const WARMUP: usize = 16;
const GPU_TIMEOUT: Duration = Duration::from_secs(30);

/// GPU samples measure render-pass execution only. With multiple frames in a
/// batch, vertex/fragment stages from different passes may overlap: a pass's
/// elapsed interval then includes pipeline latency, rather than isolated cost.
/// GPU batch spans divide the earliest-start/latest-end interval by batch size.
/// CPU samples include command encoding, staging writes, and submission, but
/// not completion waits. Wall time includes sampling-completion waits and query
/// readback, so it is conservative throughput, not an application frame time.
#[derive(Debug)]
pub(super) struct Timings {
    pub gpu_ms: Vec<f64>,
    pub gpu_batch_ms_per_frame: Vec<f64>,
    pub cpu_submit_ms: Vec<f64>,
    pub wall_ms_per_frame: f64,
}

pub(super) struct GpuBench {
    context: RenderContext,
    device_id: usize,
    renderer: Renderer,
    _background: wgpu::Texture,
    background_view: wgpu::TextureView,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    width: u32,
    height: u32,
    timestamp_period_ns: f64,
    batch_size: usize,
}

impl GpuBench {
    pub fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        ensure!(width > 0 && height > 0, "benchmark extent must be nonzero");
        let batch_size: usize = std::env::var("BENGA_MOTION_BENCH_BATCH_SIZE")
            .unwrap_or_else(|_| "1".into())
            .parse()
            .context("BENGA_MOTION_BENCH_BATCH_SIZE must be an integer in 1..=8")?;
        ensure!(
            (1..=BATCH).contains(&batch_size),
            "BENGA_MOTION_BENCH_BATCH_SIZE must be in 1..=8"
        );
        let mut context = RenderContext::new();
        let device_id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor {
                label: Some("Frozen-motion benchmark"),
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
            "adapter {} does not support GPU timestamp queries; no GPU timing was measured",
            handle.adapter().get_info().name,
        );
        let timestamp_period_ns = f64::from(handle.queue.get_timestamp_period());
        ensure!(
            timestamp_period_ns > 0.0,
            "adapter returned an invalid GPU timestamp period"
        );
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        // Vello picks the first available Rgba8Unorm/Bgra8Unorm surface format.
        // Bgra8Unorm matches the usual native Metal/Windows swapchain format.
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let background = handle.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Frozen benchmark background"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let background_view = background.create_view(&Default::default());
        let target = handle.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Frozen benchmark target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());
        // A fixed opaque neutral background removes UI/audio variability while
        // retaining the production background-composite draw in every sample.
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Initialize frozen benchmark background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &background_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.02,
                            b: 0.02,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        handle.queue.submit([encoder.finish()]);
        let queries = handle.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("Frozen benchmark pass timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: (BATCH * 2) as u32,
        });
        let bytes = (BATCH * 2 * std::mem::size_of::<u64>()) as u64;
        let resolve = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Frozen benchmark timestamp resolve"),
            size: bytes,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Frozen benchmark timestamp readback"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let renderer = Renderer::new(&handle.device, format);
        Ok(Self {
            context,
            device_id,
            renderer,
            _background: background,
            background_view,
            target,
            target_view,
            queries,
            resolve,
            readback,
            width,
            height,
            timestamp_period_ns,
            batch_size,
        })
    }

    pub fn adapter_description(&self) -> String {
        let info = self.context.devices[self.device_id].adapter().get_info();
        format!(
            "{}; {:?}; {:?}; driver={} {}; timestamp period={} ns; target={}x{} Bgra8Unorm",
            info.name,
            info.backend,
            info.device_type,
            info.driver,
            info.driver_info,
            self.timestamp_period_ns,
            self.width,
            self.height,
        )
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    fn submit_frame(&mut self, frame: Frame<'_>, query_pair: Option<u32>) {
        let handle = &self.context.devices[self.device_id];
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        if let Some(beginning) = query_pair {
            self.renderer
                .benchmark_timestamps(&self.queries, beginning, beginning + 1);
        }
        self.renderer.draw(
            handle,
            &mut encoder,
            &self.background_view,
            &self.target_view,
            Size::new(f64::from(self.width), f64::from(self.height)),
            frame,
            None,
            None,
            1.0,
            None,
        );
        handle.queue.submit([encoder.finish()]);
    }

    fn wait(&self) -> anyhow::Result<()> {
        self.context.devices[self.device_id]
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(GPU_TIMEOUT),
            })?;
        Ok(())
    }

    pub fn measure(&mut self, frame: Frame<'_>, samples: usize) -> anyhow::Result<Timings> {
        ensure!(samples > 0, "benchmark needs at least one sample");
        for index in 0..WARMUP {
            self.submit_frame(frame, None);
            if (index + 1) % BATCH == 0 {
                self.wait()?;
            }
        }
        let mut timings = Timings {
            gpu_ms: Vec::with_capacity(samples),
            gpu_batch_ms_per_frame: Vec::with_capacity(samples.div_ceil(self.batch_size)),
            cpu_submit_ms: Vec::with_capacity(samples),
            wall_ms_per_frame: 0.0,
        };
        let wall_start = Instant::now();
        while timings.gpu_ms.len() < samples {
            let count = self.batch_size.min(samples - timings.gpu_ms.len());
            for index in 0..count {
                let before = Instant::now();
                self.submit_frame(frame, Some((index * 2) as u32));
                timings
                    .cpu_submit_ms
                    .push(before.elapsed().as_secs_f64() * 1000.0);
            }
            // Metal counter sampling may not be available to a resolve submitted
            // before host-observed completion (wgpu issue #9414). Keep sampling
            // and resolve submissions separate and wait before encoding resolve.
            // This wait is outside every GPU pass timestamp interval and is
            // deliberately included in the conservative wall-throughput metric.
            self.wait()?;
            let handle = &self.context.devices[self.device_id];
            let mut encoder = handle.device.create_command_encoder(&Default::default());
            let bytes = (count * 2 * std::mem::size_of::<u64>()) as u64;
            encoder.resolve_query_set(&self.queries, 0..(count * 2) as u32, &self.resolve, 0);
            encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, bytes);
            handle.queue.submit([encoder.finish()]);
            let slice = self.readback.slice(0..bytes);
            let (tx, rx) = mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            self.wait()?;
            rx.recv_timeout(GPU_TIMEOUT)
                .context("GPU query readback callback timed out")??;
            {
                let mapped = slice.get_mapped_range()?;
                let mut first_beginning = u64::MAX;
                let mut last_end = 0;
                for (index, pair) in mapped.chunks_exact(16).enumerate() {
                    let beginning = u64::from_ne_bytes(pair[..8].try_into().unwrap());
                    let end = u64::from_ne_bytes(pair[8..].try_into().unwrap());
                    ensure!(
                        beginning != 0 && end != 0 && beginning != u64::MAX && end != u64::MAX && end >= beginning,
                        "GPU timestamps were invalid or not monotonic in batch sample {index}: begin={beginning} ({beginning:#x}), end={end} ({end:#x}); raw={:02x?}",
                        &mapped[..],
                    );
                    timings
                        .gpu_ms
                        .push((end - beginning) as f64 * self.timestamp_period_ns / 1e6);
                    first_beginning = first_beginning.min(beginning);
                    last_end = last_end.max(end);
                }
                timings.gpu_batch_ms_per_frame.push(
                    (last_end - first_beginning) as f64 * self.timestamp_period_ns
                        / 1e6
                        / count as f64,
                );
            }
            self.readback.unmap();
        }
        timings.wall_ms_per_frame = wall_start.elapsed().as_secs_f64() * 1000.0 / samples as f64;
        Ok(timings)
    }

    /// PNG readback runs separately, after timing, and handles padded GPU rows.
    pub fn write_preview(&mut self, frame: Frame<'_>, path: &Path) -> anyhow::Result<()> {
        self.submit_frame(frame, None);
        let handle = &self.context.devices[self.device_id];
        let row_bytes = self.width * 4;
        let padded_row_bytes = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bytes = u64::from(padded_row_bytes) * u64::from(self.height);
        let readback = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Frozen benchmark preview readback"),
            size: bytes,
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
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(self.height),
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
        rx.recv_timeout(GPU_TIMEOUT)
            .context("GPU preview readback callback timed out")??;
        let mut pixels = Vec::with_capacity((row_bytes * self.height) as usize);
        {
            let mapped = readback.slice(..).get_mapped_range()?;
            for row in mapped.chunks_exact(padded_row_bytes as usize) {
                pixels.extend_from_slice(&row[..row_bytes as usize]);
            }
        }
        readback.unmap();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let mut encoder =
            png::Encoder::new(BufWriter::new(File::create(path)?), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&pixels)?;
        Ok(())
    }
}
