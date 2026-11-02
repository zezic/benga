//! Bounded desktop frame-cadence history. These are host wall-clock timings,
//! not GPU execution timestamps or physical display scanout measurements.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub(super) const HISTORY_SECONDS: f32 = 8.0;
const HISTORY: Duration = Duration::from_secs(8);
const FPS_WINDOW: Duration = Duration::from_millis(500);
const PERCENTILE_WINDOW: Duration = Duration::from_secs(1);
const STALE_AFTER: Duration = Duration::from_millis(250);
const MAX_SAMPLES: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub(super) struct FrameSample {
    pub at: Instant,
    pub interval_ms: f32,
    pub cpu_ms: f32,
    /// Linear downsize used by this presented frame, before feedback updates.
    pub downsize: f32,
    /// Actual GPU model target; CPU/fallback frames render at full resolution.
    pub model_extent: Option<[u32; 2]>,
    pub backend: &'static str,
    /// Do not join the preceding sample across an idle interval or switch.
    pub break_before: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct FrameMetricsSnapshot {
    /// Number of recent frames divided by their summed elapsed intervals.
    pub fps: f32,
    pub interval_ms: f32,
    pub cpu_ms: f32,
    pub p95_ms: f32,
    pub downsize: f32,
    pub model_extent: Option<[u32; 2]>,
}

#[derive(Default)]
pub(super) struct FrameMetrics {
    samples: VecDeque<FrameSample>,
    break_pending: bool,
}

impl FrameMetrics {
    /// Record a successful presentation. The caller supplies elapsed time
    /// between successful presentation calls, including any frame-rate pacing,
    /// and host time spent handling the current redraw through submission.
    pub fn record_rendered(
        &mut self,
        at: Instant,
        frame_interval: Duration,
        cpu_work: Duration,
        backend: &'static str,
        downsize: f32,
        model_extent: Option<[u32; 2]>,
    ) {
        if self.samples.back().is_some_and(|sample| at <= sample.at) {
            self.clear();
        }
        while self
            .samples
            .front()
            .is_some_and(|sample| at.saturating_duration_since(sample.at) > HISTORY)
        {
            self.samples.pop_front();
        }
        if frame_interval.is_zero() {
            self.mark_break();
            return;
        }
        let break_before = self.break_pending
            || self
                .samples
                .back()
                .is_none_or(|sample| sample.backend != backend);
        self.samples.push_back(FrameSample {
            at,
            interval_ms: frame_interval.as_secs_f32() * 1000.0,
            cpu_ms: cpu_work.as_secs_f32() * 1000.0,
            downsize: if backend.starts_with("GPU") && downsize.is_finite() {
                downsize.max(1.0)
            } else {
                1.0
            },
            model_extent: model_extent
                .filter(|extent| backend.starts_with("GPU") && !extent.contains(&0)),
            backend,
            break_before,
        });
        self.break_pending = false;
        if self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
    }

    #[cfg(test)]
    pub fn record(
        &mut self,
        at: Instant,
        frame_interval: Duration,
        cpu_work: Duration,
        backend: &'static str,
    ) {
        self.record_rendered(at, frame_interval, cpu_work, backend, 1.0, None);
    }

    /// An occlusion, resize, unavailable surface or restart is not a slow frame.
    /// Keep the old graph for comparison but start a new timing segment.
    pub fn mark_break(&mut self) {
        self.break_pending = true;
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.mark_break();
    }

    pub fn samples(&self) -> impl Iterator<Item = &FrameSample> {
        self.samples.iter()
    }

    pub fn snapshot(&self, now: Instant) -> Option<FrameMetricsSnapshot> {
        let latest = self.samples.back()?;
        if self.break_pending || now.checked_duration_since(latest.at)? > STALE_AFTER {
            return None;
        }
        // A stack scratch area avoids allocating/sorting a new Vec on each HUD
        // refresh. Only the last second in the current timing segment is used.
        let mut intervals = [0.0_f32; MAX_SAMPLES];
        let mut count = 0;
        let mut fps_count = 0;
        let mut fps_seconds = 0.0_f64;
        for sample in self.samples.iter().rev() {
            let age = now.checked_duration_since(sample.at)?;
            if age > PERCENTILE_WINDOW {
                break;
            }
            intervals[count] = sample.interval_ms;
            count += 1;
            if age <= FPS_WINDOW {
                fps_count += 1;
                fps_seconds += f64::from(sample.interval_ms) / 1000.0;
            }
            if sample.break_before {
                break;
            }
        }
        let rank = (count * 95).div_ceil(100).saturating_sub(1);
        let (_, p95, _) = intervals[..count].select_nth_unstable_by(rank, f32::total_cmp);
        Some(FrameMetricsSnapshot {
            fps: (fps_count as f64 / fps_seconds) as f32,
            interval_ms: latest.interval_ms,
            cpu_ms: latest.cpu_ms,
            p95_ms: *p95,
            downsize: latest.downsize,
            model_extent: latest.model_extent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CPU: &str = "CPU ribbons";
    const GPU: &str = "GPU ribbons: GPU projection / shutter 2";

    #[test]
    fn fps_uses_elapsed_intervals_instead_of_averaging_instantaneous_rates() {
        let origin = Instant::now();
        let mut metrics = FrameMetrics::default();
        metrics.record(
            origin,
            Duration::from_millis(10),
            Duration::from_millis(2),
            GPU,
        );
        let now = origin + Duration::from_millis(30);
        metrics.record(
            now,
            Duration::from_millis(30),
            Duration::from_millis(3),
            GPU,
        );
        let snapshot = metrics.snapshot(now).unwrap();
        assert!((snapshot.fps - 50.0).abs() < 1e-4);
        assert!((snapshot.interval_ms - 30.0).abs() < 1e-4);
        assert!((snapshot.cpu_ms - 3.0).abs() < 1e-4);
        assert!((snapshot.p95_ms - 30.0).abs() < 1e-4);
    }

    #[test]
    fn a_real_long_redraw_stall_remains_visible() {
        let now = Instant::now();
        let mut metrics = FrameMetrics::default();
        metrics.record(
            now,
            Duration::from_millis(1250),
            Duration::from_millis(1200),
            GPU,
        );
        let sample = metrics.samples().next().unwrap();
        assert!((sample.interval_ms - 1250.0).abs() < 1e-3);
        let snapshot = metrics.snapshot(now).unwrap();
        assert!((snapshot.fps - 0.8).abs() < 1e-5);
        assert!((snapshot.p95_ms - 1250.0).abs() < 1e-3);
    }

    #[test]
    fn p95_uses_the_recent_distribution_without_hiding_the_latest_spike() {
        for spike_count in [1, 2] {
            let mut at = Instant::now();
            let mut metrics = FrameMetrics::default();
            for index in 0..20 {
                let interval =
                    Duration::from_millis(if index >= 20 - spike_count { 40 } else { 10 });
                at += interval;
                metrics.record(at, interval, Duration::ZERO, GPU);
            }
            let snapshot = metrics.snapshot(at).unwrap();
            let expected = if spike_count == 1 { 10.0 } else { 40.0 };
            assert!((snapshot.p95_ms - expected).abs() < 1e-4);
            assert!((snapshot.interval_ms - 40.0).abs() < 1e-4);
        }
    }

    #[test]
    fn lifecycle_breaks_keep_history_without_claiming_fps_during_idle() {
        let origin = Instant::now();
        let mut metrics = FrameMetrics::default();
        metrics.record(
            origin,
            Duration::from_millis(10),
            Duration::from_millis(2),
            GPU,
        );
        metrics.mark_break();
        assert!(metrics.snapshot(origin).is_none());
        assert_eq!(metrics.samples().count(), 1);
        let now = origin + Duration::from_millis(100);
        metrics.record(
            now,
            Duration::from_millis(20),
            Duration::from_millis(4),
            GPU,
        );
        assert!(metrics.samples().last().unwrap().break_before);
        assert!((metrics.snapshot(now).unwrap().fps - 50.0).abs() < 1e-4);
        assert!(metrics.snapshot(now + Duration::from_millis(251)).is_none());
        metrics.record(
            now + Duration::from_millis(1),
            Duration::ZERO,
            Duration::ZERO,
            GPU,
        );
        assert_eq!(metrics.samples().count(), 2);
        assert!(metrics.snapshot(now + Duration::from_millis(1)).is_none());
    }

    #[test]
    fn renderer_switches_keep_old_graph_samples_and_reset_current_statistics() {
        let origin = Instant::now();
        let mut metrics = FrameMetrics::default();
        metrics.record(
            origin,
            Duration::from_millis(40),
            Duration::from_millis(30),
            CPU,
        );
        let now = origin + Duration::from_millis(10);
        metrics.record(
            now,
            Duration::from_millis(10),
            Duration::from_millis(2),
            GPU,
        );
        let samples: Vec<_> = metrics.samples().collect();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].backend, CPU);
        assert_eq!(samples[1].backend, GPU);
        assert!(samples[1].break_before);
        let snapshot = metrics.snapshot(now).unwrap();
        assert!((snapshot.fps - 100.0).abs() < 1e-4);
        assert!((snapshot.p95_ms - 10.0).abs() < 1e-4);
    }

    #[test]
    fn history_has_a_time_window_and_a_hard_capacity() {
        let origin = Instant::now();
        let mut metrics = FrameMetrics::default();
        for index in 0..5000 {
            metrics.record(
                origin + Duration::from_micros(index),
                Duration::from_micros(1),
                Duration::ZERO,
                GPU,
            );
        }
        assert_eq!(metrics.samples().count(), MAX_SAMPLES);
        let now = origin + Duration::from_secs(9);
        metrics.record(now, Duration::from_millis(20), Duration::ZERO, GPU);
        assert_eq!(metrics.samples().count(), 1);
        assert!((metrics.snapshot(now).unwrap().fps - 50.0).abs() < 1e-4);
    }

    #[test]
    fn a_rolled_back_timestamp_starts_a_fresh_timeline() {
        let origin = Instant::now();
        let mut metrics = FrameMetrics::default();
        metrics.record(
            origin + Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::ZERO,
            GPU,
        );
        metrics.record(origin, Duration::from_millis(20), Duration::ZERO, GPU);
        assert_eq!(metrics.samples().count(), 1);
        assert!(metrics.samples().next().unwrap().break_before);
        assert!((metrics.snapshot(origin).unwrap().fps - 50.0).abs() < 1e-4);
    }

    #[test]
    fn resolution_history_stays_attached_to_its_presented_frame() {
        let origin = Instant::now();
        let mut metrics = FrameMetrics::default();
        for (index, downsize, extent, interval) in [
            (0, 2.0, [2560, 1440], 16),
            (1, 8.0, [640, 360], 30),
            (2, 4.0, [1280, 720], 17),
        ] {
            metrics.record_rendered(
                origin + Duration::from_millis(index * 30),
                Duration::from_millis(interval),
                Duration::from_millis(5),
                GPU,
                downsize,
                Some(extent),
            );
        }
        let samples: Vec<_> = metrics.samples().collect();
        assert_eq!(
            samples.iter().map(|s| s.downsize).collect::<Vec<_>>(),
            [2.0, 8.0, 4.0]
        );
        assert_eq!(samples[1].interval_ms, 30.0);
        assert_eq!(samples[1].model_extent, Some([640, 360]));
        assert!(
            !samples[1].break_before,
            "quality changes remain connected on the timeline"
        );
        let latest = metrics
            .snapshot(origin + Duration::from_millis(60))
            .unwrap();
        assert_eq!(latest.downsize, 4.0);
        assert_eq!(latest.model_extent, Some([1280, 720]));

        // A stale adaptive factor cannot label the CPU renderer as downsized.
        let now = origin + Duration::from_millis(90);
        metrics.record_rendered(
            now,
            Duration::from_millis(16),
            Duration::from_millis(5),
            CPU,
            8.0,
            Some([640, 360]),
        );
        let latest = metrics.snapshot(now).unwrap();
        assert_eq!(latest.downsize, 1.0);
        assert_eq!(latest.model_extent, None);
    }
}
