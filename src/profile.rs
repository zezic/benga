use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use cpu_time::{ProcessTime, ThreadTime};
use vello::wgpu;
use winit::monitor::MonitorHandle;

use crate::{
    frame_schedule::FrameRateLimit,
    render::{GpuFrameTiming, Presentation, Stage},
    visualizer::Ui,
    AudioEvent,
};

pub const SAMPLE_RATE: usize = 48000;
const WARMUP: Duration = Duration::from_secs(1);
const MEASURE: Duration = Duration::from_secs(10);
const MAX_SAMPLES: usize = 8192;

struct Options {
    limit: FrameRateLimit,
    extent: Option<[u32; 2]>,
    downsize: Option<f32>,
    performance_overlay: bool,
    present_mode: Option<wgpu::PresentMode>,
}

impl Options {
    fn parse(
        fps: Option<&str>,
        extent: Option<&str>,
        downsize: Option<&str>,
        performance_overlay: Option<&str>,
        present_mode: Option<&str>,
    ) -> anyhow::Result<Self> {
        let limit = match fps.unwrap_or("240") {
            "15" => FrameRateLimit::Fps15,
            "30" => FrameRateLimit::Fps30,
            "48" => FrameRateLimit::Fps48,
            "55" => FrameRateLimit::Fps55,
            "60" => FrameRateLimit::Fps60,
            "72" => FrameRateLimit::Fps72,
            "90" => FrameRateLimit::Fps90,
            "120" => FrameRateLimit::Fps120,
            "180" => FrameRateLimit::Fps180,
            "240" => FrameRateLimit::Fps240,
            _ => bail!("BENGA_PROFILE_FPS must be 15, 30, 48, 55, 60, 72, 90, 120, 180, or 240"),
        };
        let extent = extent
            .map(|value| -> anyhow::Result<[u32; 2]> {
                let (width, height) = value
                    .split_once('x')
                    .context("BENGA_PROFILE_EXTENT must be WIDTHxHEIGHT in physical pixels")?;
                let size = [width.parse::<u32>()?, height.parse::<u32>()?];
                ensure!(
                    size.iter().all(|v| *v > 0 && *v <= 16_384),
                    "BENGA_PROFILE_EXTENT dimensions must be between 1 and 16384 physical pixels"
                );
                Ok(size)
            })
            .transpose()?;
        let downsize = downsize
            .map(|value| -> anyhow::Result<f32> {
                let value = value
                    .parse::<f32>()
                    .context("invalid BENGA_PROFILE_RIBBON_DOWNSIZE")?;
                ensure!(
                    value.is_finite() && value >= 1.0,
                    "BENGA_PROFILE_RIBBON_DOWNSIZE must be finite and at least 1"
                );
                Ok(value)
            })
            .transpose()?;
        let performance_overlay = match performance_overlay {
            Some("1" | "true") => true,
            None | Some("0" | "false") => false,
            _ => bail!("BENGA_PROFILE_PERFORMANCE_OVERLAY must be 0, 1, false, or true"),
        };
        let present_mode = match present_mode {
            None => None,
            Some("auto_vsync") => Some(wgpu::PresentMode::AutoVsync),
            Some("fifo") => Some(wgpu::PresentMode::Fifo),
            Some("immediate") => Some(wgpu::PresentMode::Immediate),
            _ => bail!("BENGA_PROFILE_PRESENT_MODE must be auto_vsync, fifo, or immediate"),
        };
        Ok(Self {
            limit,
            extent,
            downsize,
            performance_overlay,
            present_mode,
        })
    }

    fn from_env() -> anyhow::Result<Self> {
        let read = |name: &str| -> anyhow::Result<Option<String>> {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(error) => Err(error).with_context(|| format!("invalid {name}")),
            }
        };
        let fps = read("BENGA_PROFILE_FPS")?;
        let extent = read("BENGA_PROFILE_EXTENT")?;
        let downsize = read("BENGA_PROFILE_RIBBON_DOWNSIZE")?;
        let performance_overlay = read("BENGA_PROFILE_PERFORMANCE_OVERLAY")?;
        let present_mode = read("BENGA_PROFILE_PRESENT_MODE")?;
        Self::parse(
            fps.as_deref(),
            extent.as_deref(),
            downsize.as_deref(),
            performance_overlay.as_deref(),
            present_mode.as_deref(),
        )
    }
}

enum Phase {
    Pending,
    Warmup(Instant),
    Measuring {
        run_start: Instant,
        wall_start: Instant,
        process_start: ProcessTime,
        thread_start: ThreadTime,
    },
}

#[derive(Default)]
struct Stages {
    acquire: Duration,
    rasterize: Duration,
    submit: Duration,
}

struct Sample {
    cadence: Option<Duration>,
    work: Duration,
    stages: Stages,
}

fn timing_summary(values: impl Iterator<Item = Duration>) -> Option<[f64; 3]> {
    let mut values: Vec<f64> = values.map(|v| v.as_secs_f64() * 1000.0).collect();
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let percentile =
        |percent: usize| values[(values.len() * percent).div_ceil(100).saturating_sub(1)];
    Some([percentile(50), percentile(95), *values.last().unwrap()])
}

#[derive(PartialEq, Eq)]
struct PacingKey {
    monitor: Option<MonitorHandle>,
    reported_refresh_millihertz: Option<u32>,
    adaptive_budget: Duration,
    present_mode: wgpu::PresentMode,
    desired_maximum_frame_latency: u32,
    extent: [u32; 2],
}

struct PacingMetadata {
    key: PacingKey,
    monitor_name: Option<String>,
    supported_present_modes: Vec<wgpu::PresentMode>,
}

impl PacingMetadata {
    fn report(&self, selected: FrameRateLimit) {
        let budget = self.key.adaptive_budget.as_secs_f64();
        let reported_hz = self
            .key
            .reported_refresh_millihertz
            .map(|rate| f64::from(rate) / 1000.0);
        eprintln!(
            "Profile pacing: selected {selected} FPS; effective adaptive budget {:.3} ms ({:.1} FPS); monitor {:?}; reported refresh {:?} Hz; configured present mode {:?}; desired maximum frame latency {}; supported present modes {:?}; surface {}x{}",
            budget * 1000.0,
            1.0 / budget,
            self.monitor_name,
            reported_hz,
            self.key.present_mode,
            self.key.desired_maximum_frame_latency,
            self.supported_present_modes,
            self.key.extent[0],
            self.key.extent[1],
        );
    }
}

/// A bounded synthetic workload through the normal window and presentation loop.
pub struct Run {
    options: Options,
    phase: Phase,
    audio_frames: usize,
    frames: u32,
    rasterized_frames: u32,
    draw: Duration,
    stages: Stages,
    previous_presentation: Option<Instant>,
    samples: Vec<Sample>,
    pacing: Option<PacingMetadata>,
    gpu_timing_supported: Option<bool>,
    gpu_samples: Vec<Duration>,
    gpu_model_samples: Vec<Duration>,
    gpu_frame_samples: Vec<GpuFrameTiming>,
}

impl Run {
    pub fn from_args() -> anyhow::Result<Option<Self>> {
        let mut args = std::env::args_os().skip(1);
        match (args.next(), args.next()) {
            (None, None) => Ok(None),
            (Some(arg), None) if arg == "--profile" => Ok(Some(Self {
                options: Options::from_env()?,
                phase: Phase::Pending,
                audio_frames: 0,
                frames: 0,
                rasterized_frames: 0,
                draw: Duration::ZERO,
                stages: Stages::default(),
                previous_presentation: None,
                samples: Vec::with_capacity(2400),
                pacing: None,
                gpu_timing_supported: None,
                gpu_samples: Vec::with_capacity(2400),
                gpu_model_samples: Vec::with_capacity(2400),
                gpu_frame_samples: Vec::with_capacity(2400),
            })),
            _ => bail!("Usage: benga [--profile]"),
        }
    }

    pub fn target_frame_rate(&self) -> FrameRateLimit {
        self.options.limit
    }

    pub fn requested_extent(&self) -> Option<[u32; 2]> {
        self.options.extent
    }

    pub fn ribbon_downsize(&self) -> Option<f32> {
        self.options.downsize
    }

    pub fn performance_overlay(&self) -> bool {
        self.options.performance_overlay
    }

    pub fn requested_present_mode(&self) -> Option<wgpu::PresentMode> {
        self.options.present_mode
    }

    pub fn pause_timing(&mut self) {
        self.previous_presentation = None;
    }

    pub fn record_gpu_timing_support(&mut self, supported: bool) {
        if self.gpu_timing_supported != Some(supported) {
            eprintln!("GPU timestamp feedback supported: {supported}; asynchronous queue-render span, no completion wait");
            self.gpu_timing_supported = Some(supported);
        }
    }

    pub fn record_gpu_frame_timing(&mut self, timing: &GpuFrameTiming) {
        if matches!(self.phase, Phase::Measuring { .. })
            && self.gpu_samples.len() < MAX_SAMPLES
            && !timing.elapsed.is_zero()
        {
            self.gpu_samples.push(timing.elapsed);
            self.gpu_model_samples.push(timing.model_elapsed);
            self.gpu_frame_samples.push(*timing);
        }
    }

    /// Capture display/surface configuration only when it changes. Monitor
    /// names and supported-mode vectors are not allocated on every frame.
    pub fn record_pacing_metadata(
        &mut self,
        monitor: Option<MonitorHandle>,
        reported_refresh_millihertz: Option<u32>,
        adaptive_budget: Duration,
        config: &wgpu::SurfaceConfiguration,
        supported_present_modes: impl FnOnce() -> Vec<wgpu::PresentMode>,
    ) {
        let key = PacingKey {
            monitor,
            reported_refresh_millihertz,
            adaptive_budget,
            present_mode: config.present_mode,
            desired_maximum_frame_latency: config.desired_maximum_frame_latency,
            extent: [config.width, config.height],
        };
        if self.pacing.as_ref().is_some_and(|prior| prior.key == key) {
            return;
        }
        let metadata = PacingMetadata {
            monitor_name: key.monitor.as_ref().and_then(MonitorHandle::name),
            key,
            supported_present_modes: supported_present_modes(),
        };
        metadata.report(self.options.limit);
        self.pacing = Some(metadata);
    }

    pub fn feed(&mut self, ui: &mut Ui, now: Instant) -> anyhow::Result<()> {
        let start = match &self.phase {
            Phase::Pending => {
                self.phase = Phase::Warmup(now);
                now
            }
            Phase::Warmup(start) => {
                let start = *start;
                if now.duration_since(start) >= WARMUP {
                    self.phase = Phase::Measuring {
                        run_start: start,
                        wall_start: now,
                        process_start: ProcessTime::try_now()?,
                        thread_start: ThreadTime::try_now()?,
                    };
                }
                start
            }
            Phase::Measuring { run_start, .. } => *run_start,
        };
        let elapsed = now.duration_since(start).as_secs_f64();
        let due = (elapsed * SAMPLE_RATE as f64) as usize;
        // Deliver only new samples, including when presentation limits the cadence.
        ui.audio_event(AudioEvent::Buffer {
            buf: vec![levels(elapsed); due - self.audio_frames],
            captured_at: start
                + Duration::from_secs_f64(self.audio_frames as f64 / SAMPLE_RATE as f64),
        });
        self.audio_frames = due;
        Ok(())
    }

    pub fn present(
        &mut self,
        frame_started: Instant,
        draw: Instant,
        after_draw: Instant,
        changed: bool,
        render: impl FnOnce(&mut dyn FnMut(Stage)) -> anyhow::Result<Presentation>,
    ) -> anyhow::Result<Presentation> {
        let mut stages = Stages::default();
        // Surface acquisition starts at the render call, not at the end of UI
        // construction. This excludes the caller's intervening frame setup.
        let mut previous = Instant::now();
        let result = render(&mut |stage| {
            let now = Instant::now();
            let elapsed = now - previous;
            previous = now;
            match stage {
                Stage::Acquire => stages.acquire = elapsed,
                Stage::Rasterize => stages.rasterize = elapsed,
                Stage::Submit => stages.submit = elapsed,
            }
        })?;
        let completed = Instant::now();
        let cadence = if matches!(result, Presentation::Presented) {
            self.previous_presentation
                .replace(completed)
                .map(|previous| completed.duration_since(previous))
        } else {
            self.pause_timing();
            None
        };
        if matches!(self.phase, Phase::Measuring { .. })
            && matches!(result, Presentation::Presented)
        {
            self.frames += 1;
            self.rasterized_frames += u32::from(changed);
            self.draw += after_draw - draw;
            self.stages.acquire += stages.acquire;
            self.stages.rasterize += stages.rasterize;
            self.stages.submit += stages.submit;
            if self.samples.len() < MAX_SAMPLES {
                self.samples.push(Sample {
                    cadence,
                    work: completed.duration_since(frame_started),
                    stages,
                });
            }
        }
        Ok(result)
    }

    pub fn finished(&self, now: Instant) -> bool {
        match &self.phase {
            Phase::Pending => false,
            Phase::Warmup(start)
            | Phase::Measuring {
                run_start: start, ..
            } => now.duration_since(*start) >= WARMUP + MEASURE,
        }
    }

    pub fn report(&self) -> anyhow::Result<()> {
        ensure!(
            self.frames > 0,
            "the profiling window presented no measured frames"
        );
        let Phase::Measuring {
            wall_start,
            process_start,
            thread_start,
            ..
        } = &self.phase
        else {
            unreachable!("measured frames have CPU and wall-clock origins")
        };
        let elapsed = wall_start.elapsed().as_secs_f64();
        let process_cpu = process_start.try_elapsed()?.as_secs_f64();
        let main_cpu = thread_start.try_elapsed()?.as_secs_f64();
        let frames = f64::from(self.frames);
        eprintln!(
            "Benga profile: {:.1} FPS; {}/{} frames rasterized; draw {:.1} us/frame; present {:.1} us/frame",
            frames / elapsed,
            self.rasterized_frames,
            self.frames,
            self.draw.as_secs_f64() * 1e6 / frames,
            (self.stages.acquire + self.stages.rasterize + self.stages.submit).as_secs_f64() * 1e6
                / frames,
        );
        eprintln!(
            "CPU: process {:.1}%; main thread {:.1}%; acquire {:.1} us/frame; rasterize {:.1} us/frame; submit {:.1} us/frame",
            100.0 * process_cpu / elapsed,
            100.0 * main_cpu / elapsed,
            self.stages.acquire.as_secs_f64() * 1e6 / frames,
            self.stages.rasterize.as_secs_f64() * 1e6 / frames,
            self.stages.submit.as_secs_f64() * 1e6 / frames,
        );
        let budget = self
            .options
            .limit
            .interval()
            .expect("profile FPS limit is explicit");
        eprintln!("Profile target: {} FPS ({:.3} ms); requested physical extent {:?}; fixed ribbon downsize {:?}; performance overlay {}; requested present mode {:?}", self.options.limit, budget.as_secs_f64() * 1000.0, self.options.extent, self.options.downsize, self.options.performance_overlay, self.options.present_mode);
        if let Some(pacing) = &self.pacing {
            pacing.report(self.options.limit);
        }
        eprintln!(
            "GPU timestamp feedback supported: {}; {} completed queue-render samples received during measurement",
            self.gpu_timing_supported.unwrap_or(false),
            self.gpu_samples.len(),
        );
        if let Some([p50, p95, max]) = timing_summary(self.gpu_samples.iter().copied()) {
            eprintln!("GPU queue-render span ms: p50 {p50:.3}; p95 {p95:.3}; max {max:.3}. Scope begins after acquisition and ends before presentation; it includes CPU feed gaps and is not busy GPU utilization. Readback is asynchronous and may trail submission by several frames.");
        }
        if let Some([p50, p95, max]) = timing_summary(self.gpu_model_samples.iter().copied()) {
            eprintln!("GPU ribbon-pass span ms: p50 {p50:.3}; p95 {p95:.3}; max {max:.3}");
        }
        if let Some([p50, p95, max]) = timing_summary(
            self.gpu_samples
                .iter()
                .zip(&self.gpu_model_samples)
                .map(|(total, model)| total.saturating_sub(*model)),
        ) {
            eprintln!("GPU queue span outside ribbon pass ms: p50 {p50:.3}; p95 {p95:.3}; max {max:.3}. Derived per frame from the paired total/model timestamps; includes CPU feed gaps.");
        }
        let describe = |name: &str, summary: Option<[f64; 3]>| {
            if let Some([p50, p95, max]) = summary {
                eprintln!("Host {name} ms: p50 {p50:.3}; p95 {p95:.3}; max {max:.3}");
            }
        };
        describe(
            "cadence",
            timing_summary(self.samples.iter().filter_map(|s| s.cadence)),
        );
        describe(
            "redraw work",
            timing_summary(self.samples.iter().map(|s| s.work)),
        );
        describe(
            "active redraw excluding acquisition",
            timing_summary(
                self.samples
                    .iter()
                    .map(|s| s.work.saturating_sub(s.stages.acquire)),
            ),
        );
        describe(
            "surface acquisition",
            timing_summary(self.samples.iter().map(|s| s.stages.acquire)),
        );
        describe(
            "rasterization",
            timing_summary(self.samples.iter().map(|s| s.stages.rasterize)),
        );
        describe(
            "encode/submit",
            timing_summary(self.samples.iter().map(|s| s.stages.submit)),
        );
        let cadence_count = self.samples.iter().filter(|s| s.cadence.is_some()).count();
        let cadence_over = self
            .samples
            .iter()
            .filter(|s| s.cadence.is_some_and(|v| v > budget))
            .count();
        let work_over = self.samples.iter().filter(|s| s.work > budget).count();
        eprintln!("Over requested {:.3} ms budget: cadence {cadence_over}/{cadence_count}; redraw work {work_over}/{}. Host stages include waits/backpressure and are not GPU execution timestamps.", budget.as_secs_f64() * 1000.0, self.samples.len());
        self.write_trace(budget)?;
        Ok(())
    }

    /// Optional diagnostic export occurs after measurement and does no file
    /// access or serialization on the presentation path.
    fn write_trace(&self, requested_budget: Duration) -> anyhow::Result<()> {
        let Some(path) = std::env::var_os("BENGA_PROFILE_TRACE_OUTPUT") else {
            return Ok(());
        };
        let path = std::path::PathBuf::from(path);
        let pacing = self.pacing.as_ref().map(|p| {
            serde_json::json!({
                "monitor_name":p.monitor_name,
                "reported_refresh_millihertz":p.key.reported_refresh_millihertz,
                "adaptive_budget_ns":p.key.adaptive_budget.as_nanos(),
                "configured_present_mode":format!("{:?}",p.key.present_mode),
                "desired_maximum_frame_latency":p.key.desired_maximum_frame_latency,
                "supported_present_modes":p.supported_present_modes.iter().map(|mode|format!("{mode:?}")).collect::<Vec<_>>(),
                "surface_extent":p.key.extent,
            })
        });
        let trace = serde_json::json!({
            "schema_version":1,
            "scope":"Bounded samples received during the measured profile phase. GPU queue spans exclude acquisition/presentation, include CPU feed gaps, and can trail submission by several frames; they are not busy GPU utilization.",
            "selected_fps":self.options.limit.to_string().parse::<u32>()?,
            "requested_budget_ns":requested_budget.as_nanos(),
            "effective_budget_ns":self.pacing.as_ref().map_or(requested_budget,|p|p.key.adaptive_budget).as_nanos(),
            "pacing":pacing,
            "fixed_ribbon_downsize":self.options.downsize,
            "performance_overlay":self.options.performance_overlay,
            "gpu_timing_supported":self.gpu_timing_supported.unwrap_or(false),
            "gpu_samples":self.gpu_frame_samples.iter().map(|sample|serde_json::json!({
                "tag":{"downsize":sample.tag.downsize,"viewport":sample.tag.viewport,"generation":sample.tag.generation},
                "elapsed_ns":sample.elapsed.as_nanos(),
                "model_elapsed_ns":sample.model_elapsed.as_nanos(),
                "outside_model_elapsed_ns":sample.elapsed.saturating_sub(sample.model_elapsed).as_nanos(),
            })).collect::<Vec<_>>(),
            "host_samples":self.samples.iter().map(|sample|serde_json::json!({
                "cadence_ns":sample.cadence.map(|value|value.as_nanos()),
                "redraw_ns":sample.work.as_nanos(),
                "acquisition_ns":sample.stages.acquire.as_nanos(),
                "active_redraw_ns":sample.work.saturating_sub(sample.stages.acquire).as_nanos(),
                "rasterization_ns":sample.stages.rasterize.as_nanos(),
                "submit_ns":sample.stages.submit.as_nanos(),
            })).collect::<Vec<_>>(),
        });
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "could not create profile trace directory {}",
                    parent.display()
                )
            })?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&trace)?)
            .with_context(|| format!("could not write profile trace {}", path.display()))?;
        eprintln!("Profile trace: {}", path.display());
        Ok(())
    }
}

pub fn levels(elapsed: f64) -> [f32; 2] {
    let phase = elapsed * std::f64::consts::TAU;
    [
        (0.02 + 0.9 * (phase.sin() * 0.5 + 0.5)) as f32,
        (0.02 + 0.9 * (phase.cos() * 0.5 + 0.5)) as f32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_controls_parse_without_changing_saved_settings() {
        let defaults = Options::parse(None, None, None, None, None).unwrap();
        assert_eq!(defaults.limit, FrameRateLimit::Fps240);
        assert_eq!(defaults.extent, None);
        assert_eq!(defaults.downsize, None);
        assert!(!defaults.performance_overlay);
        assert_eq!(defaults.present_mode, None);
        let selected =
            Options::parse(Some("60"), Some("5120x2880"), Some("5120"), Some("0"), None).unwrap();
        assert_eq!(selected.limit, FrameRateLimit::Fps60);
        assert_eq!(selected.extent, Some([5120, 2880]));
        assert_eq!(selected.downsize, Some(5120.0));
        assert!(!selected.performance_overlay);
        assert!(
            Options::parse(None, None, None, Some("1"), None)
                .unwrap()
                .performance_overlay
        );
        for (fps, extent, downsize) in [
            (Some("61"), None, None),
            (None, Some("0x2880"), None),
            (None, Some("5120"), None),
            (None, None, Some("NaN")),
            (None, None, Some("0.5")),
        ] {
            assert!(Options::parse(fps, extent, downsize, None, None).is_err());
        }
        assert!(Options::parse(None, None, None, Some("invalid"), None).is_err());
        for (name, expected) in [
            ("auto_vsync", wgpu::PresentMode::AutoVsync),
            ("fifo", wgpu::PresentMode::Fifo),
            ("immediate", wgpu::PresentMode::Immediate),
        ] {
            assert_eq!(
                Options::parse(None, None, None, None, Some(name))
                    .unwrap()
                    .present_mode,
                Some(expected)
            );
        }
        assert!(Options::parse(None, None, None, None, Some("mailbox")).is_err());
    }

    #[test]
    fn host_timing_percentiles_keep_the_peak_and_handle_empty_segments() {
        assert_eq!(timing_summary(std::iter::empty()), None);
        let values = (1..=20).map(Duration::from_millis);
        assert_eq!(timing_summary(values), Some([10.0, 19.0, 20.0]));
    }
}
