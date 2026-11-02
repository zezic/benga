//! Small, retained-text performance graphs for the opt-in renderer trial.
use std::time::{Duration, Instant};

use vello::{
    kurbo::{Affine, BezPath, Line, Point, Rect, Size, Stroke},
    peniko::{Color, Fill},
    Scene,
};

use crate::{
    text::{Fonts, Text},
    visualizer::frame_metrics::{FrameMetrics, HISTORY_SECONDS},
};

const WIDTH: f64 = 340.0;
const HEIGHT: f64 = 330.0;
const REFRESH: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct PerformanceHud {
    labels: Option<Labels>,
    refreshed: Option<Instant>,
    status: &'static str,
    width: f64,
    scale: f64,
    buckets: Vec<Bucket>,
    downsize_ceiling: u32,
    quality: Option<(f32, Option<[u32; 2]>)>,
}

struct Labels {
    status: Text,
    switch: Text,
    gpu: Text,
    cpu: Text,
    fps: Text,
    frame: Text,
    p95: Text,
    footer: Text,
    fps_ticks: [Text; 3],
    time_ticks: [Text; 4],
    fps_max: Text,
    frame_max: Text,
    downsize: Text,
    extent: Text,
    downsize_hint: Text,
    downsize_ticks: Vec<(u32, Text)>,
}

#[derive(Clone, Copy, Default)]
struct Bucket {
    count: usize,
    fps: Range,
    frame: Range,
    cpu: Range,
    downsize: Range,
    gpu: bool,
    break_before: bool,
}

#[derive(Clone, Copy, Default)]
struct Range {
    low: f32,
    high: f32,
    last: f32,
}

impl Range {
    fn include(&mut self, value: f32, first: bool) {
        if first {
            self.low = value;
            self.high = value;
        } else {
            self.low = self.low.min(value);
            self.high = self.high.max(value);
        }
        self.last = value;
    }
}

fn is_gpu(status: &str) -> bool {
    status.starts_with("GPU")
}

fn backend_color(gpu: bool, light: bool) -> Color {
    match (gpu, light) {
        (true, false) => Color::from_rgb8(89, 218, 193),
        (true, true) => Color::from_rgb8(0, 113, 100),
        (false, false) => Color::from_rgb8(255, 184, 91),
        (false, true) => Color::from_rgb8(158, 81, 0),
    }
}

fn y(rect: Rect, value: f32, maximum: f32) -> f64 {
    rect.y1 - f64::from(value / maximum) * rect.height()
}

fn downsize_label(value: f32) -> String {
    if value < 1_000_000.0 {
        format!("{value:.2}x")
    } else {
        format!("{value:.2e}x")
    }
}

fn downsize_ticks(ceiling: u32) -> Vec<u32> {
    let step = ceiling.div_ceil(3).max(1) as usize;
    let mut ticks: Vec<_> = (0..=ceiling).step_by(step).collect();
    if ticks.last() != Some(&ceiling) {
        ticks.push(ceiling);
    }
    ticks
}

fn line(scene: &mut Scene, transform: Affine, color: Color, width: f64, a: Point, b: Point) {
    scene.stroke(
        &Stroke::new(width),
        transform,
        color,
        None,
        &Line::new(a, b),
    );
}

impl PerformanceHud {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        fonts: &mut Fonts,
        metrics: &FrameMetrics,
        viewport: Size,
        scale: f64,
        background: Color,
        ink: Color,
        status: &'static str,
        now: Instant,
    ) {
        // Keep the whole overlay within short windows while honoring normal UI
        // scale on desktop-size surfaces. Coordinates below are logical pixels.
        let scale = scale.min(viewport.height / (HEIGHT + 54.0)).max(0.25);
        let width = WIDTH.min(viewport.width / scale - 24.0);
        if width < 150.0 || viewport.height < 95.0 {
            return;
        }
        let transform = Affine::translate((12.0 * scale, 42.0 * scale)) * Affine::scale(scale);
        let fps_plot = Rect::new(30.0, 62.0, width - 8.0, 116.0);
        let frame_plot = Rect::new(30.0, 151.0, width - 8.0, 205.0);
        let downsize_plot = Rect::new(30.0, 250.0, width - 8.0, 301.0);
        let columns = fps_plot.width().floor() as usize + 1;
        self.buckets.resize(columns, Bucket::default());
        self.buckets.fill(Bucket::default());
        let mut fps_max = 120.0_f32;
        let mut frame_max = 33.333_336_f32;
        let mut downsize_max = 4.0_f32;
        for sample in metrics.samples() {
            let age = now.saturating_duration_since(sample.at).as_secs_f32();
            if age > HISTORY_SECONDS || sample.at > now || sample.interval_ms <= 0.0 {
                continue;
            }
            let column = ((1.0 - age / HISTORY_SECONDS) * (columns - 1) as f32).round() as usize;
            let bucket = &mut self.buckets[column.min(columns - 1)];
            let first = bucket.count == 0;
            let fps = 1000.0 / sample.interval_ms;
            bucket.fps.include(fps, first);
            bucket.frame.include(sample.interval_ms, first);
            bucket.cpu.include(sample.cpu_ms, first);
            let downsize = if sample.downsize.is_finite() {
                sample.downsize.max(1.0)
            } else {
                1.0
            };
            bucket.downsize.include(downsize, first);
            downsize_max = downsize_max.max(downsize);
            bucket.gpu = is_gpu(sample.backend);
            bucket.break_before |= sample.break_before;
            bucket.count += 1;
            fps_max = fps_max.max(fps);
            frame_max = frame_max.max(sample.interval_ms).max(sample.cpu_ms);
        }
        // Leave a little headroom while always preserving the largest spike.
        fps_max *= 1.05;
        frame_max *= 1.05;
        let downsize_ceiling = downsize_max.log2().ceil() as u32;
        // Inspect the latest quality cheaply each frame. Percentiles and text
        // layout still run only when a label refresh is actually necessary.
        let quality = metrics
            .samples()
            .last()
            .filter(|sample| {
                sample.at <= now
                    && now.saturating_duration_since(sample.at) <= REFRESH
                    && sample.backend == status
            })
            .map(|sample| (sample.downsize, sample.model_extent));
        let refresh = self.labels.is_none()
            || self.status != status
            || self.width != width
            || self.scale != scale
            || self.downsize_ceiling != downsize_ceiling
            || self.quality != quality
            || self
                .refreshed
                .is_none_or(|at| now.saturating_duration_since(at) >= REFRESH);
        if refresh {
            let snapshot = metrics.snapshot(now);
            let (fps, frame, p95, cpu) = snapshot.map_or_else(
                || {
                    (
                        "FPS  --".to_owned(),
                        "FRAME TIME  -- ms".to_owned(),
                        "p95 -- ms".to_owned(),
                        "--".to_owned(),
                    )
                },
                |s| {
                    (
                        format!("FPS  {:.1}", s.fps),
                        format!("FRAME TIME  {:.1} ms", s.interval_ms),
                        format!("p95 {:.1} ms", s.p95_ms),
                        format!("{:.1}", s.cpu_ms),
                    )
                },
            );
            let (downsize, extent) = snapshot.map_or_else(
                || ("DOWNSIZE  --".to_owned(), "--".to_owned()),
                |s| {
                    (
                        format!("DOWNSIZE  {}", downsize_label(s.downsize)),
                        s.model_extent
                            .map_or_else(|| "full res".to_owned(), |[w, h]| format!("{w}x{h}")),
                    )
                },
            );
            // A narrow window still receives a complete status line. Only its
            // font is reduced; graph values retain their usual size.
            let status_size = 11.0 * ((width - 16.0) / 260.0).min(1.0);
            self.labels = Some(Labels {
                status: fonts.layout(status, status_size as f32),
                switch: fonts.layout(
                    if width >= 240.0 {
                        "R: switch renderer"
                    } else {
                        "R: switch"
                    },
                    10.0,
                ),
                gpu: fonts.layout("GPU", 10.0),
                cpu: fonts.layout("CPU", 10.0),
                fps: fonts.layout(&fps, 13.0),
                frame: fonts.layout(&frame, (12.0 * ((width - 16.0) / 190.0).min(1.0)) as f32),
                p95: fonts.layout(&p95, 9.0),
                footer: fonts.layout(
                    &if width >= 260.0 {
                        format!("8 s   thin: CPU to submit {cpu} ms")
                    } else {
                        format!("8 s   CPU {cpu} ms")
                    },
                    9.0,
                ),
                fps_max: fonts.layout(&format!("max {:.0}", fps_max), 8.0),
                frame_max: fonts.layout(&format!("max {:.0} ms", frame_max), 8.0),
                fps_ticks: ["0", "60", "120"].map(|label| fonts.layout(label, 8.0)),
                time_ticks: ["0", "8.3", "16.7", "33.3"].map(|label| fonts.layout(label, 8.0)),
                downsize: fonts.layout(
                    &downsize,
                    (11.0 * ((width - 16.0) / 240.0).min(1.0)).max(8.0) as f32,
                ),
                extent: fonts.layout(&extent, 9.0),
                downsize_hint: fonts.layout("up = lower resolution (log2)", 8.0),
                downsize_ticks: downsize_ticks(downsize_ceiling)
                    .into_iter()
                    .map(|exponent| {
                        (
                            exponent,
                            fonts.layout(
                                &if exponent < 20 {
                                    format!("{}x", 1u64 << exponent)
                                } else {
                                    format!("2^{exponent}x")
                                },
                                8.0,
                            ),
                        )
                    })
                    .collect(),
            });
            self.refreshed = Some(now);
            self.status = status;
            self.width = width;
            self.scale = scale;
            self.downsize_ceiling = downsize_ceiling;
            self.quality = snapshot.map(|s| (s.downsize, s.model_extent));
        }
        let labels = self
            .labels
            .as_ref()
            .expect("performance labels initialized");
        let panel = Rect::new(0.0, 0.0, width, HEIGHT);
        scene.fill(
            Fill::NonZero,
            transform,
            background.with_alpha(0.97),
            None,
            &panel,
        );
        scene.stroke(
            &Stroke::new(0.7),
            transform,
            ink.with_alpha(0.22),
            None,
            &panel,
        );
        labels
            .status
            .draw_left(scene, transform * Affine::translate((8.0, 15.0)), ink);
        labels.switch.draw_left(
            scene,
            transform * Affine::translate((8.0, 31.0)),
            ink.with_alpha(0.65),
        );
        let light = ink.to_rgba8().r < 128;
        let gpu_color = backend_color(true, light);
        let cpu_color = backend_color(false, light);
        labels.gpu.draw_left(
            scene,
            transform * Affine::translate((width - 68.0, 31.0)),
            gpu_color,
        );
        labels.cpu.draw_left(
            scene,
            transform * Affine::translate((width - 32.0, 31.0)),
            cpu_color,
        );
        // The colored legend is independent of the current backend: earlier
        // samples remain useful immediately after pressing R.
        labels
            .fps
            .draw_left(scene, transform * Affine::translate((8.0, 53.0)), ink);
        labels
            .frame
            .draw_left(scene, transform * Affine::translate((8.0, 142.0)), ink);
        if width >= 260.0 {
            labels.p95.draw_left(
                scene,
                transform * Affine::translate((width - labels.p95.width() - 8.0, 53.0)),
                ink.with_alpha(0.7),
            );
        }
        labels
            .downsize
            .draw_left(scene, transform * Affine::translate((8.0, 230.0)), ink);
        labels.extent.draw_left(
            scene,
            transform * Affine::translate((width - labels.extent.width() - 8.0, 230.0)),
            ink.with_alpha(0.7),
        );
        labels.downsize_hint.draw_left(
            scene,
            transform * Affine::translate((downsize_plot.x0, 244.0)),
            ink.with_alpha(0.6),
        );
        labels.footer.draw_left(
            scene,
            transform * Affine::translate((8.0, 321.0)),
            ink.with_alpha(0.65),
        );

        for (plot, maximum, ticks, texts) in [
            (
                fps_plot,
                fps_max,
                &[0.0, 60.0, 120.0][..],
                &labels.fps_ticks[..],
            ),
            (
                frame_plot,
                frame_max,
                &[0.0, 8.333_333, 16.666_666, 33.333_332][..],
                &labels.time_ticks[..],
            ),
        ] {
            let mut last_label_y = f64::INFINITY;
            for (&tick, text) in ticks.iter().zip(texts) {
                let yy = y(plot, tick, maximum);
                line(
                    scene,
                    transform,
                    ink.with_alpha(0.13),
                    0.6,
                    Point::new(plot.x0, yy),
                    Point::new(plot.x1, yy),
                );
                if last_label_y - yy >= 11.0 {
                    text.draw_left(
                        scene,
                        transform * Affine::translate((plot.x0 - text.width() - 4.0, yy + 2.5)),
                        ink.with_alpha(0.55),
                    );
                    last_label_y = yy;
                }
            }
        }
        for (exponent, text) in &labels.downsize_ticks {
            let yy = y(downsize_plot, *exponent as f32, downsize_ceiling as f32);
            line(
                scene,
                transform,
                ink.with_alpha(0.13),
                0.6,
                Point::new(downsize_plot.x0, yy),
                Point::new(downsize_plot.x1, yy),
            );
            text.draw_left(
                scene,
                transform * Affine::translate((downsize_plot.x0 - text.width() - 4.0, yy + 2.5)),
                ink.with_alpha(0.55),
            );
        }
        for (plot, text) in [(fps_plot, &labels.fps_max), (frame_plot, &labels.frame_max)] {
            text.draw_left(
                scene,
                transform * Affine::translate((plot.x1 - text.width(), plot.y0 - 2.0)),
                ink.with_alpha(0.55),
            );
        }
        self.trace(
            scene,
            transform,
            fps_plot,
            fps_max,
            gpu_color,
            cpu_color,
            ink,
            |bucket| bucket.fps,
            false,
            false,
        );
        self.trace(
            scene,
            transform,
            frame_plot,
            frame_max,
            gpu_color,
            cpu_color,
            ink,
            |bucket| bucket.frame,
            false,
            false,
        );
        self.trace(
            scene,
            transform,
            frame_plot,
            frame_max,
            gpu_color,
            cpu_color,
            ink,
            |bucket| bucket.cpu,
            true,
            false,
        );
        self.trace(
            scene,
            transform,
            downsize_plot,
            downsize_ceiling as f32,
            gpu_color,
            cpu_color,
            ink,
            |bucket| Range {
                low: bucket.downsize.low.log2(),
                high: bucket.downsize.high.log2(),
                last: bucket.downsize.last.log2(),
            },
            false,
            true,
        );
        // Switching backends and external idle breaks have a common vertical
        // marker. Curve segments never interpolate through these boundaries.
        for (column, _bucket) in self
            .buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| b.count > 0 && b.break_before)
        {
            let x = fps_plot.x0 + column as f64;
            for plot in [fps_plot, frame_plot, downsize_plot] {
                line(
                    scene,
                    transform,
                    ink.with_alpha(0.3),
                    0.7,
                    Point::new(x, plot.y0),
                    Point::new(x, plot.y1),
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn trace(
        &self,
        scene: &mut Scene,
        transform: Affine,
        plot: Rect,
        maximum: f32,
        gpu_color: Color,
        cpu_color: Color,
        ink: Color,
        value: impl Fn(&Bucket) -> Range,
        cpu: bool,
        stepped: bool,
    ) {
        let mut path = BezPath::new();
        let mut previous_gpu = None;
        let mut previous_column = None;
        let mut previous_point: Option<Point> = None;
        for (column, bucket) in self.buckets.iter().enumerate().filter(|(_, b)| b.count > 0) {
            let range = value(bucket);
            let xx = plot.x0 + column as f64;
            let color = if cpu {
                ink.with_alpha(0.5)
            } else if bucket.gpu {
                gpu_color
            } else {
                cpu_color
            };
            let point = Point::new(xx, y(plot, range.last, maximum));
            let disconnected = bucket.break_before
                || previous_gpu.is_some_and(|gpu| gpu != bucket.gpu)
                || previous_column.is_some_and(|prior| column - prior > 12);
            if disconnected {
                let previous_color = if cpu {
                    ink.with_alpha(0.5)
                } else if previous_gpu.unwrap_or(false) {
                    gpu_color
                } else {
                    cpu_color
                };
                scene.stroke(
                    &Stroke::new(if cpu { 0.8 } else { 1.1 }),
                    transform,
                    previous_color,
                    None,
                    &path,
                );
                path = BezPath::new();
            }
            if path.elements().is_empty() {
                path.move_to(point);
            } else {
                if stepped {
                    if let Some(prior) = previous_point {
                        path.line_to(Point::new(point.x, prior.y));
                    }
                }
                path.line_to(point);
            }
            // A per-column min/max stroke retains subpixel-duration hitches and
            // avoids drawing thousands of redundant history line segments.
            line(
                scene,
                transform,
                color,
                if cpu { 0.7 } else { 1.0 },
                Point::new(xx, y(plot, range.low, maximum)),
                Point::new(xx, y(plot, range.high, maximum)),
            );
            previous_gpu = Some(bucket.gpu);
            previous_column = Some(column);
            previous_point = Some(point);
        }
        let color = if cpu {
            ink.with_alpha(0.5)
        } else if previous_gpu.unwrap_or(false) {
            gpu_color
        } else {
            cpu_color
        };
        scene.stroke(
            &Stroke::new(if cpu { 0.8 } else { 1.1 }),
            transform,
            color,
            None,
            &path,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GPU: &str = "GPU ribbons: GPU projection / shutter 2";
    const CPU: &str = "CPU ribbons";

    #[test]
    fn pixel_buckets_keep_hitches_and_text_refresh_is_bounded() -> anyhow::Result<()> {
        let now = Instant::now();
        let mut metrics = FrameMetrics::default();
        metrics.record_rendered(
            now - Duration::from_millis(1),
            Duration::from_millis(80),
            Duration::from_millis(40),
            GPU,
            16.0,
            Some([320, 180]),
        );
        metrics.record_rendered(
            now,
            Duration::from_millis(1),
            Duration::from_millis(1),
            GPU,
            2.0,
            Some([2560, 1440]),
        );
        let mut fonts = Fonts::new()?;
        let mut hud = PerformanceHud::default();
        let mut scene = Scene::new();
        let mut draw = |hud: &mut PerformanceHud, at, status| {
            hud.draw(
                &mut scene,
                &mut fonts,
                &metrics,
                Size::new(640.0, 360.0),
                1.0,
                Color::BLACK,
                Color::WHITE,
                status,
                at,
            );
        };
        draw(&mut hud, now, GPU);
        let populated: Vec<_> = hud.buckets.iter().filter(|b| b.count > 0).collect();
        assert_eq!(
            populated.len(),
            1,
            "two subpixel samples share one time bucket"
        );
        assert_eq!(populated[0].frame.low, 1.0);
        assert_eq!(
            populated[0].frame.high, 80.0,
            "downsampling retains the hitch"
        );
        assert_eq!(populated[0].frame.last, 1.0);
        assert_eq!(populated[0].downsize.low, 2.0);
        assert_eq!(
            populated[0].downsize.high, 16.0,
            "a brief resolution cut remains visible"
        );
        assert_eq!(populated[0].downsize.last, 2.0);
        assert_eq!(hud.quality, Some((2.0, Some([2560, 1440]))));
        draw(&mut hud, now + Duration::from_millis(100), GPU);
        assert_eq!(
            hud.refreshed,
            Some(now),
            "text stays cached between quarter-second updates"
        );
        draw(&mut hud, now + Duration::from_millis(250), GPU);
        assert_eq!(hud.refreshed, Some(now + Duration::from_millis(250)));
        draw(&mut hud, now + Duration::from_millis(251), CPU);
        assert_eq!(hud.status, CPU, "backend status refreshes immediately");
        Ok(())
    }

    #[test]
    fn quality_changes_refresh_the_current_extent_without_waiting_for_text_timer(
    ) -> anyhow::Result<()> {
        let now = Instant::now();
        let mut metrics = FrameMetrics::default();
        metrics.record_rendered(
            now,
            Duration::from_millis(16),
            Duration::from_millis(4),
            GPU,
            2.0,
            Some([2560, 1440]),
        );
        let mut fonts = Fonts::new()?;
        let mut hud = PerformanceHud::default();
        let draw = |hud: &mut PerformanceHud, fonts: &mut Fonts, metrics: &FrameMetrics, at| {
            hud.draw(
                &mut Scene::new(),
                fonts,
                metrics,
                Size::new(640.0, 360.0),
                1.0,
                Color::BLACK,
                Color::WHITE,
                GPU,
                at,
            );
        };
        draw(&mut hud, &mut fonts, &metrics, now);
        let later = now + Duration::from_millis(16);
        metrics.record_rendered(
            later,
            Duration::from_millis(16),
            Duration::from_millis(4),
            GPU,
            4.0,
            Some([1280, 720]),
        );
        draw(&mut hud, &mut fonts, &metrics, later);
        assert_eq!(hud.refreshed, Some(later));
        assert_eq!(hud.quality, Some((4.0, Some([1280, 720]))));
        assert_eq!(downsize_ticks(2), vec![0, 1, 2]);
        assert!(downsize_ticks(13).len() <= 5);
        assert_eq!(downsize_ticks(13).last(), Some(&13));
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU; writes dark/light/narrow/HiDPI HUD previews"]
    fn renders_performance_hud_previews() -> anyhow::Result<()> {
        use std::{fs::File, io::BufWriter, sync::mpsc};
        use vello::{util::RenderContext, wgpu, RenderParams};
        let mut context = RenderContext::new();
        let id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor::default(),
        ))?;
        let handle = &context.devices[id];
        let mut renderer = crate::render::renderer(&handle.device)?;
        let mut fonts = Fonts::new()?;
        let origin = Instant::now();
        let mut elapsed = Duration::ZERO;
        let mut metrics = FrameMetrics::default();
        for frame in 0..720 {
            let gpu = frame >= 240;
            let downsize = match frame {
                0..=239 => 1.0,
                240..=359 => 1.0,
                360..=419 => 2.0,
                420..=479 => 4.0,
                480..=539 => 8.0,
                540..=599 => 4.0,
                600..=659 => 2.0,
                _ => 1.0,
            };
            let interval = if frame == 480 {
                Duration::from_millis(80)
            } else {
                Duration::from_secs_f64(match frame {
                    0..=239 => 1.0 / 60.0,
                    360..=419 => 0.022,
                    420..=479 => 0.014,
                    _ => 1.0 / 120.0,
                })
            };
            elapsed += interval;
            metrics.record_rendered(
                origin + elapsed,
                interval,
                interval.mul_f64(0.36),
                if gpu { GPU } else { CPU },
                downsize,
                gpu.then(|| {
                    [
                        (5120.0 / downsize).ceil() as u32,
                        (2880.0 / downsize).ceil() as u32,
                    ]
                }),
            );
        }
        let now = origin + elapsed;
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/gpu-ribbon-integration/performance-hud");
        std::fs::create_dir_all(&output)?;
        for (name, width, height, scale, light) in [
            ("dark", 640_u32, 360_u32, 1.0, false),
            ("light", 640, 360, 1.0, true),
            ("narrow", 240, 360, 1.0, false),
            ("hidpi", 1280, 720, 2.0, false),
        ] {
            let background = if light {
                Color::from_rgb8(239, 240, 244)
            } else {
                Color::from_rgb8(18, 19, 26)
            };
            let ink = if light {
                Color::from_rgb8(30, 32, 40)
            } else {
                Color::from_rgb8(237, 241, 247)
            };
            let mut scene = Scene::new();
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                background,
                None,
                &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            );
            PerformanceHud::default().draw(
                &mut scene,
                &mut fonts,
                &metrics,
                Size::new(f64::from(width), f64::from(height)),
                scale,
                background,
                ink,
                GPU,
                now,
            );
            let texture = handle.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Performance HUD preview"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            renderer.render_to_texture(
                &handle.device,
                &handle.queue,
                &scene,
                &texture.create_view(&Default::default()),
                &RenderParams {
                    base_color: Color::TRANSPARENT,
                    width,
                    height,
                    antialiasing_method: crate::render::ANTIALIASING,
                },
            )?;
            let padded = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let readback = handle.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Performance HUD preview readback"),
                size: u64::from(padded * height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = handle.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: Some(height),
                    },
                },
                texture.size(),
            );
            handle.queue.submit([encoder.finish()]);
            let (tx, rx) = mpsc::channel();
            readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            handle.device.poll(wgpu::PollType::wait_indefinitely())?;
            rx.recv()??;
            let pixels: Vec<_> = readback
                .slice(..)
                .get_mapped_range()?
                .chunks_exact(padded as usize)
                .flat_map(|row| row[..width as usize * 4].iter().copied())
                .collect();
            readback.unmap();
            assert!(pixels
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != pixels[..3]));
            let mut png = png::Encoder::new(
                BufWriter::new(File::create(output.join(format!("{name}.png")))?),
                width,
                height,
            );
            png.set_color(png::ColorType::Rgba);
            png.set_depth(png::BitDepth::Eight);
            png.write_header()?.write_image_data(&pixels)?;
        }
        Ok(())
    }
}
