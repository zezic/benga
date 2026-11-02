//! Opt-in host-side slow-frame attribution. No GPU queries or readback.
use std::time::{Duration, Instant};

use benga::render::Stage;

const SLOW_FRAME: Duration = Duration::from_millis(50);
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct FrameTrace {
    extent: Option<(u32, u32, f64)>,
    last_report: Option<Instant>,
    pending_count: usize,
    worst: Option<SlowFrame>,
}

pub struct Capture {
    started: Instant,
    draw: Option<(Instant, Instant)>,
    previous_stage: Option<Instant>,
    acquire: Option<Duration>,
    rasterize: Option<Duration>,
    submit: Option<Duration>,
}

#[derive(Clone, Copy)]
struct Timings {
    wall: Duration,
    cadence: Option<Duration>,
    pre_ui: Duration,
    ui: Duration,
    acquire: Option<Duration>,
    rasterize: Option<Duration>,
    submit: Option<Duration>,
    other: Duration,
}

struct SlowFrame {
    timings: Timings,
    presented: bool,
    backend: &'static str,
    model: String,
    extent: Option<(u32, u32, f64)>,
}

impl Capture {
    pub fn new(started: Instant) -> Self {
        Self {
            started,
            draw: None,
            previous_stage: None,
            acquire: None,
            rasterize: None,
            submit: None,
        }
    }

    pub fn ui_draw(&mut self, started: Instant, ended: Instant) {
        self.draw = Some((started, ended));
    }

    pub fn begin_present(&mut self) {
        self.previous_stage = Some(Instant::now());
    }

    pub fn stage(&mut self, stage: &Stage) {
        self.stage_at(stage, Instant::now());
    }

    fn stage_at(&mut self, stage: &Stage, now: Instant) {
        let elapsed = now.saturating_duration_since(self.previous_stage.unwrap_or(self.started));
        self.previous_stage = Some(now);
        match stage {
            Stage::Acquire => self.acquire = Some(elapsed),
            Stage::Rasterize => self.rasterize = Some(elapsed),
            Stage::Submit => self.submit = Some(elapsed),
        }
    }

    fn timings(&self, completed: Instant, previous: Option<Instant>, presented: bool) -> Timings {
        let wall = completed.saturating_duration_since(self.started);
        let (pre_ui, ui) = self
            .draw
            .map_or((Duration::ZERO, Duration::ZERO), |(start, end)| {
                (
                    start.saturating_duration_since(self.started),
                    end.saturating_duration_since(start),
                )
            });
        let accounted = pre_ui
            + ui
            + self.acquire.unwrap_or_default()
            + self.rasterize.unwrap_or_default()
            + self.submit.unwrap_or_default();
        Timings {
            wall,
            cadence: presented
                .then(|| previous.and_then(|at| completed.checked_duration_since(at)))
                .flatten(),
            pre_ui,
            ui,
            acquire: self.acquire,
            rasterize: self.rasterize,
            submit: self.submit,
            other: wall.saturating_sub(accounted),
        }
    }
}

impl Timings {
    fn severity(self) -> Duration {
        self.wall.max(self.cadence.unwrap_or_default())
    }
}

impl FrameTrace {
    pub fn from_env(experimental: bool) -> Option<Self> {
        (experimental && std::env::var_os("BENGA_TRACE_SLOW_FRAMES").is_some_and(|v| v == "1"))
            .then(Self::default)
    }

    pub fn extent(&mut self, width: u32, height: u32, scale: f64) {
        let extent = (width, height, scale);
        if self.extent == Some(extent) {
            return;
        }
        self.extent = Some(extent);
        eprintln!("Frame timing: target={width}x{height} physical pixels; UI scale={scale:.2}; slow threshold=50 ms; host timings, no GPU readback");
    }

    pub fn record(
        &mut self,
        capture: Capture,
        completed: Instant,
        previous: Option<Instant>,
        presented: bool,
        backend: &'static str,
        model: &str,
    ) {
        let timings = capture.timings(completed, previous, presented);
        if timings.severity() >= SLOW_FRAME {
            self.pending_count += 1;
            if self
                .worst
                .as_ref()
                .is_none_or(|old| timings.severity() > old.timings.severity())
            {
                self.worst = Some(SlowFrame {
                    timings,
                    presented,
                    backend,
                    model: model.to_owned(),
                    extent: self.extent,
                });
            }
        }
        if self
            .last_report
            .is_none_or(|at| completed.saturating_duration_since(at) >= REPORT_INTERVAL)
            && self.pending_count > 0
        {
            self.flush();
            self.last_report = Some(completed);
        }
    }

    pub fn flush(&mut self) {
        if let Some(frame) = self.worst.take() {
            eprintln!("{}", format_report(self.pending_count, &frame));
        }
        self.pending_count = 0;
    }
}

fn milliseconds(value: Duration) -> f64 {
    value.as_secs_f64() * 1000.0
}
fn optional_ms(value: Option<Duration>) -> String {
    value.map_or_else(|| "--".to_owned(), |v| format!("{:.2}", milliseconds(v)))
}

fn format_report(count: usize, frame: &SlowFrame) -> String {
    let t = frame.timings;
    let target = frame.extent.map_or_else(
        || "unknown".to_owned(),
        |(width, height, _)| format!("{width}x{height}"),
    );
    let gap = t.cadence.map(|cadence| cadence.saturating_sub(t.wall));
    format!("Slow frames: count={count}; worst target={target}; backend={}; model={:?}; result={}; cadence={} ms; redraw={:.2} ms; outside-redraw={} ms; pre-UI={:.2} ms; Ui.draw={:.2} ms; acquire={} ms; rasterize={} ms; encode-submit={} ms; other={:.2} ms",
        frame.backend, frame.model, if frame.presented { "presented" } else { "surface-unavailable" },
        optional_ms(t.cadence), milliseconds(t.wall), optional_ms(gap),
        milliseconds(t.pre_ui), milliseconds(t.ui), optional_ms(t.acquire),
        optional_ms(t.rasterize), optional_ms(t.submit), milliseconds(t.other))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_retains_its_worst_frame_and_counts_without_logging_each_one() {
        let origin = Instant::now();
        let mut trace = FrameTrace {
            last_report: Some(origin),
            ..Default::default()
        };
        for (start_ms, wall_ms) in [(0, 80), (100, 60), (200, 170)] {
            let started = origin + Duration::from_millis(start_ms);
            let capture = Capture::new(started);
            trace.record(
                capture,
                started + Duration::from_millis(wall_ms),
                None,
                true,
                "GPU ribbons",
                "M249",
            );
        }
        assert_eq!(trace.pending_count, 3);
        assert_eq!(
            trace.worst.as_ref().unwrap().timings.wall,
            Duration::from_millis(170)
        );
        assert_eq!(trace.last_report, Some(origin));
    }

    #[test]
    fn stages_account_for_host_work_without_calling_acquire_gpu_time() {
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let mut capture = Capture::new(origin);
        capture.ui_draw(at(3), at(7));
        capture.previous_stage = Some(at(8));
        capture.stage_at(&Stage::Acquire, at(88));
        capture.stage_at(&Stage::Rasterize, at(98));
        capture.stage_at(&Stage::Submit, at(101));
        let t = capture.timings(at(102), Some(origin - Duration::from_millis(20)), true);
        assert_eq!(t.wall, Duration::from_millis(102));
        assert_eq!(t.cadence, Some(Duration::from_millis(122)));
        assert_eq!(t.pre_ui, Duration::from_millis(3));
        assert_eq!(t.ui, Duration::from_millis(4));
        assert_eq!(t.acquire, Some(Duration::from_millis(80)));
        assert_eq!(t.rasterize, Some(Duration::from_millis(10)));
        assert_eq!(t.submit, Some(Duration::from_millis(3)));
        assert_eq!(t.other, Duration::from_millis(2));
    }

    #[test]
    fn unavailable_surface_has_no_present_cadence_or_invented_gpu_stages() {
        let origin = Instant::now();
        let mut capture = Capture::new(origin);
        capture.ui_draw(origin, origin + Duration::from_millis(1));
        let t = capture.timings(origin + Duration::from_millis(80), Some(origin), false);
        assert_eq!(t.cadence, None);
        assert_eq!(t.acquire, None);
        assert_eq!(t.rasterize, None);
        assert_eq!(t.submit, None);
        let message = format_report(
            2,
            &SlowFrame {
                timings: t,
                presented: false,
                backend: "GPU ribbons",
                model: "M249".to_owned(),
                extent: Some((5120, 2880, 2.0)),
            },
        );
        assert!(message.contains("target=5120x2880"));
        assert!(message.contains("result=surface-unavailable"));
        assert!(message.contains("cadence=--"));
        assert!(message.contains("acquire=--"));
        assert!(!message.contains("GPU time"));
    }
}
