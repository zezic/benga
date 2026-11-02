//! Per-frame host timing feedback for the experimental model layer. No GPU readback.
use std::time::Duration;

pub(super) const BASE_DOWNSIZE: f32 = 1.0;
const DROP_AT: f64 = 0.85;
const RESTORE_BELOW: f64 = 0.65;
const AIM_FOR: f64 = 0.75;

#[derive(Clone, Copy)]
struct Adjustment {
    previous_downsize: f32,
    previous_cost: f64,
    previous_effect_cost: f64,
    finer: bool,
}

pub(super) struct AdaptiveResolution {
    enabled: bool,
    downsize: f32,
    maximum: f32,
    target: Duration,
    previous_interval: Option<f64>,
    previous_observation: Option<(f64, f32, bool)>,
    long_gap: bool,
    adjustment: Option<Adjustment>,
    unproductive: u8,
    plateau_start: (f64, f32),
    plateau_effect_cost: f64,
    held_cost: Option<f64>,
    /// Rejected candidate and prior safe cost. Retry a smaller quality step
    /// instead of either repeating the unsafe jump or blocking all recovery.
    rejected_finer: Option<(f32, f64)>,
}

impl Default for AdaptiveResolution {
    fn default() -> Self {
        Self {
            enabled: false,
            downsize: BASE_DOWNSIZE,
            maximum: f32::MAX,
            target: Duration::ZERO,
            previous_interval: None,
            previous_observation: None,
            long_gap: false,
            adjustment: None,
            unproductive: 0,
            plateau_start: (0.0, BASE_DOWNSIZE),
            plateau_effect_cost: 0.0,
            held_cost: None,
            rejected_finer: None,
        }
    }
}

impl AdaptiveResolution {
    pub fn enable(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.restart();
        }
    }

    pub fn downsize(&self) -> f32 {
        if self.enabled {
            self.downsize
        } else {
            1.0
        }
    }

    pub fn extent(&mut self, viewport: [u32; 2]) -> [u32; 2] {
        self.maximum = viewport[0].max(viewport[1]).max(1) as f32;
        self.downsize = self.downsize.min(self.maximum);
        extent(viewport, self.downsize())
    }

    pub fn reset_timing(&mut self) {
        self.previous_interval = None;
        self.previous_observation = None;
        self.long_gap = false;
        self.adjustment = None;
        self.unproductive = 0;
        self.held_cost = None;
        self.rejected_finer = None;
    }

    pub fn restart(&mut self) {
        self.downsize = BASE_DOWNSIZE;
        self.reset_timing();
    }

    /// Roll back a native GPU trial that proved insensitive to resolution.
    pub fn cap_downsize(&mut self, limit: f32) {
        let limit = limit.max(BASE_DOWNSIZE).min(self.maximum);
        if self.downsize > limit {
            self.downsize = limit;
            self.reset_timing();
        }
    }

    pub fn record(&mut self, interval: Duration, work: Duration, target: Duration) {
        self.record_with_acquisition(interval, work, Duration::ZERO, target);
    }

    pub fn record_with_acquisition(
        &mut self,
        interval: Duration,
        work: Duration,
        acquisition_wait: Duration,
        target: Duration,
    ) {
        self.record_measurement(interval, work, acquisition_wait, target, None);
    }

    /// Native GPU feedback separates the control budget from the actual model
    /// pass used to evaluate whether changing resolution helped.
    pub fn record_gpu_cost(&mut self, cost: Duration, model_cost: Duration, target: Duration) {
        self.record_measurement(target, cost, Duration::ZERO, target, Some(model_cost));
    }

    fn record_measurement(
        &mut self,
        interval: Duration,
        work: Duration,
        acquisition_wait: Duration,
        target: Duration,
        model_cost: Option<Duration>,
    ) {
        if !self.enabled || target.is_zero() || interval.is_zero() {
            return;
        }
        if self.target != target {
            self.restart();
            self.target = target;
        }
        let interval = interval.as_secs_f64();
        let total_work = work.as_secs_f64();
        // nextDrawable can wait for presentation even when model rendering is
        // cheap. That wait is not CPU/raster work that resolution can remove.
        let work = work.saturating_sub(acquisition_wait).as_secs_f64();
        let budget = target.as_secs_f64();
        // An isolated idle gap is not render pressure. Repeated long intervals
        // can still represent asynchronous GPU overload outside the callback.
        if interval > 0.25 && total_work < interval * 0.5 {
            if !self.long_gap {
                self.reset_timing();
                self.long_gap = true;
                return;
            }
        } else {
            self.long_gap = false;
        }
        let previous_interval = self.previous_interval.replace(interval).unwrap_or(budget);
        // VSync/limiter cadence is not render cost. Only use cadence as pressure
        // when the CURRENT interval is late and its adjacent-frame mean is late.
        // A current cheap/healthy frame can therefore restore quality at once.
        let cadence_mean = (interval + previous_interval) * 0.5;
        let cadence_pressure = interval > budget * 1.12 && cadence_mean > budget * 1.12;
        let cost = if cadence_pressure {
            // Use the paired delivery period, rather than alternating early/
            // late presentation calls. Their jitter must not masquerade as a
            // successful resolution cut and reset the fixed-cost guard.
            work.max(cadence_mean)
        } else {
            work
        };
        let gpu_cost = model_cost.is_some();
        let effect_cost = model_cost.map_or(cost, |cost| cost.as_secs_f64());
        let previous = self
            .previous_observation
            .replace((effect_cost, self.downsize, gpu_cost));
        let projected_cost = match previous {
            Some((prior_cost, prior_downsize, was_gpu))
                if self.downsize >= prior_downsize && was_gpu == gpu_cost =>
            {
                // If cost grows while quality is unchanged or coarser, scene
                // pressure is increasing. Reserve for the next frame's rise.
                let projected_effect = effect_cost + (effect_cost - prior_cost).max(0.0);
                if gpu_cost && effect_cost > 0.0 {
                    cost * (projected_effect / effect_cost)
                } else {
                    projected_effect
                }
            }
            // A finer target can itself explain a cost increase. Do not turn
            // that intended quality improvement into artificial scene growth.
            _ => cost,
        };

        if let Some(adjustment) = self.adjustment.take() {
            if adjustment.finer {
                self.unproductive = 0;
                if cost > budget * DROP_AT && effect_cost > adjustment.previous_effect_cost * 1.12 {
                    let rejected = self.downsize;
                    self.downsize = adjustment.previous_downsize.min(self.maximum);
                    self.rejected_finer = Some((rejected, adjustment.previous_cost));
                    // The finer trial proved that resolution affects cost.
                    // A previous fixed-cost hold must not veto useful cuts.
                    self.held_cost = None;
                    self.unproductive = 0;
                    return;
                }
                if cost > budget * DROP_AT {
                    // Finer quality costs roughly the same while a fixed host
                    // cost prevents meeting the target. Keep the extra detail.
                    self.held_cost = Some(cost);
                } else {
                    self.held_cost = None;
                }
            } else {
                if self.unproductive == 0 {
                    self.plateau_start = (adjustment.previous_cost, adjustment.previous_downsize);
                    self.plateau_effect_cost = adjustment.previous_effect_cost;
                }
                // Judge the entire cut sequence against its anchor. A lower
                // jitter sample must not restart an otherwise unhelpful run.
                if effect_cost <= self.plateau_effect_cost * 0.90 {
                    self.unproductive = 0;
                } else {
                    self.unproductive += 1;
                    if self.unproductive >= 3 {
                        self.downsize = self.plateau_start.1.min(self.maximum);
                        self.held_cost = Some(self.plateau_start.0.max(cost));
                        self.unproductive = 0;
                        return;
                    }
                }
            }
        }
        if self.held_cost.is_some_and(|held| {
            let rising_limit = if held < budget {
                held + (budget - held) * 0.25
            } else {
                held * 1.25
            };
            projected_cost > rising_limit || cost < held * 0.90
        }) {
            self.held_cost = None;
            self.rejected_finer = None;
        }
        if cost < budget * RESTORE_BELOW {
            self.held_cost = None;
        }
        if self
            .rejected_finer
            .is_some_and(|(_, prior)| cost < prior * 0.90)
        {
            self.rejected_finer = None;
        }

        let finer = self.downsize > BASE_DOWNSIZE
            && (projected_cost < budget * RESTORE_BELOW || self.held_cost.is_some());
        let mut next = if finer {
            // Pixel cost scales approximately with inverse downsize squared.
            // A fixed-cost hold can also restore detail without reaching an
            // otherwise unattainable FPS target first.
            let change = if self.held_cost.is_some() && projected_cost >= budget * RESTORE_BELOW {
                0.5
            } else {
                (projected_cost / (budget * AIM_FOR)).sqrt().clamp(0.5, 1.0)
            };
            (self.downsize * change as f32).max(BASE_DOWNSIZE)
        } else if projected_cost >= budget * DROP_AT && self.held_cost.is_none() {
            let change = (projected_cost / (budget * AIM_FOR)).sqrt().clamp(1.0, 2.0);
            (self.downsize * change as f32).min(self.maximum)
        } else {
            self.downsize
        };
        if finer {
            if let Some((rejected, _)) = self.rejected_finer {
                if rejected < self.downsize {
                    next = next.max((self.downsize + rejected) * 0.5);
                }
            }
        }
        if next != self.downsize {
            self.adjustment = Some(Adjustment {
                previous_downsize: self.downsize,
                previous_cost: cost,
                previous_effect_cost: effect_cost,
                finer: next < self.downsize,
            });
            self.downsize = next;
        }
    }
}

pub(super) fn extent(viewport: [u32; 2], downsize: f32) -> [u32; 2] {
    let downsize = if downsize.is_finite() {
        downsize.max(1.0)
    } else {
        1.0
    };
    viewport.map(|dimension| ((dimension as f32 / downsize).ceil() as u32).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(controller: &mut AdaptiveResolution, frames: usize, interval: f64, work: f64) {
        for _ in 0..frames {
            controller.record(
                Duration::from_secs_f64(interval),
                Duration::from_secs_f64(work),
                Duration::from_secs_f64(1.0 / 60.0),
            );
        }
    }

    fn feed_pixel_cost(controller: &mut AdaptiveResolution, frames: usize, pixels: f64) {
        for _ in 0..frames {
            let downsize = f64::from(controller.downsize());
            let cost = 0.002 + pixels / (downsize * downsize);
            feed(controller, 1, cost.max(1.0 / 60.0), cost);
        }
    }

    #[test]
    fn starts_at_full_extent_and_never_makes_empty_textures() {
        let mut controller = AdaptiveResolution::default();
        assert_eq!(controller.extent([1920, 1080]), [1920, 1080]);
        controller.enable(true);
        assert_eq!(controller.extent([1921, 1081]), [1921, 1081]);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
        assert_eq!(extent([1, 1], 1000.0), [1, 1]);
    }

    #[test]
    fn cheap_frames_restore_unity_without_supersampling() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        let viewport = [5120, 2880];
        controller.extent(viewport);
        feed_pixel_cost(&mut controller, 32, 2.5);
        let coarse = controller.downsize();
        assert!(coarse > BASE_DOWNSIZE);
        feed(&mut controller, 1, 1.0 / 60.0, 0.004);
        assert!(
            controller.downsize() < coarse,
            "the first cheap frame restores detail"
        );
        feed(&mut controller, 16, 1.0 / 60.0, 0.004);
        assert_eq!(controller.downsize(), 1.0);
        assert_eq!(controller.extent(viewport), viewport);
        feed(&mut controller, 120, 1.0 / 60.0, 0.0001);
        assert_eq!(
            controller.downsize(),
            1.0,
            "headroom never requests supersampling"
        );
        assert_eq!(controller.extent(viewport), viewport);
    }

    #[test]
    fn quality_restarts_and_unhelpful_cut_rollbacks_allow_unity() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([1920, 1080]);
        feed(&mut controller, 1, 1.0 / 60.0, 0.020);
        assert!(controller.downsize() > 1.0);
        controller.cap_downsize(1.0);
        assert_eq!(controller.downsize(), 1.0);
        feed(&mut controller, 1, 1.0 / 60.0, 0.020);
        assert!(controller.downsize() > 1.0);
        controller.restart();
        assert_eq!(controller.downsize(), 1.0);
        assert_eq!(controller.extent([1920, 1080]), [1920, 1080]);
    }

    #[test]
    fn jittered_120_hz_presentation_cannot_collapse_quality_at_higher_fps_targets() {
        for fps in [180.0, 240.0] {
            let budget = Duration::from_secs_f64(1.0 / fps);
            for work_ms in [3, 4, 5] {
                let mut controller = AdaptiveResolution::default();
                controller.enable(true);
                controller.extent([5120, 2880]);

                // The display/presenter delivers 120 FPS with modest adjacent
                // jitter. Render work is constant and independent of quality;
                // reducing the model target cannot accelerate this pacing.
                for frame in 0..24 {
                    let interval = if frame % 2 == 0 {
                        0.0075
                    } else {
                        1.0 / 60.0 - 0.0075
                    };
                    controller.record(
                        Duration::from_secs_f64(interval),
                        Duration::from_millis(work_ms),
                        budget,
                    );
                }
                assert_eq!(
                    controller.downsize(),
                    BASE_DOWNSIZE,
                    "a higher requested FPS must not turn presentation pacing into permanent quality loss: target={fps} FPS, constant work={work_ms} ms, actual target={:?}",
                    controller.extent([5120, 2880]),
                );
            }
        }
    }

    #[test]
    fn fixed_work_budget_jitter_cannot_reset_unhelpful_model_cut_evidence() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2818]);
        let budget = Duration::from_secs_f64(1.0 / 120.0);
        let mut worst = BASE_DOWNSIZE;
        for cost in [8, 6].into_iter().cycle().take(240) {
            // Fixed full-frame work changes the model allowance, but the
            // measured model itself costs the same at every resolution.
            controller.record_gpu_cost(
                Duration::from_millis(cost),
                Duration::from_millis(2),
                budget,
            );
            worst = worst.max(controller.downsize());
        }
        assert!(worst < BASE_DOWNSIZE * 2.0, "holding quality between cuts must not erase proof that cuts did not improve model time: {worst}");
    }

    #[test]
    fn acquisition_wait_jitter_at_a_presentation_ceiling_preserves_useful_quality() {
        for fps in [180.0, 240.0] {
            let budget = Duration::from_secs_f64(1.0 / fps);
            let active_work = Duration::from_millis(3);
            let mut controller = AdaptiveResolution::default();
            controller.enable(true);
            controller.extent([5120, 2880]);
            for frame in 0..24 {
                let interval = Duration::from_secs_f64(if frame % 2 == 0 {
                    0.0075
                } else {
                    1.0 / 60.0 - 0.0075
                });
                let acquisition_wait = interval - active_work;
                controller.record_with_acquisition(interval, interval, acquisition_wait, budget);
                assert_ne!(
                    controller.extent([5120, 2880]),
                    [1, 1],
                    "presentation wait must not collapse quality: requested={fps} FPS, frame={frame}",
                );
            }
            assert_eq!(
                controller.downsize(),
                BASE_DOWNSIZE,
                "3 ms of actual work should retain useful quality when unchanged presenter delivery prevents a higher FPS: requested={fps} FPS",
            );
        }
    }

    #[test]
    fn acquisition_accounting_still_reacts_to_a_real_near_deadline_redraw() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        let initial = controller.extent([5120, 2880]);
        let budget = Duration::from_secs_f64(1.0 / 60.0);
        controller.record_with_acquisition(
            budget,
            Duration::from_secs_f64(0.0148),
            Duration::ZERO,
            budget,
        );
        assert!(controller.downsize() > BASE_DOWNSIZE);
        let reduced = controller.extent([5120, 2880]);
        assert!(reduced[0] < initial[0] && reduced[1] < initial[1]);
    }

    #[test]
    fn genuine_gpu_backpressure_still_cuts_and_a_current_healthy_frame_restores() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        let budget = Duration::from_secs_f64(1.0 / 60.0);
        // CPU encoding is cheap, but delivery is genuinely behind the target.
        // Subtracting acquisition wait must not discard that cadence signal.
        controller.record_with_acquisition(
            Duration::from_secs_f64(1.0 / 30.0),
            Duration::from_millis(31),
            Duration::from_millis(29),
            budget,
        );
        let coarse = controller.downsize();
        assert!(coarse > BASE_DOWNSIZE);
        controller.record_with_acquisition(
            budget,
            Duration::from_millis(4),
            Duration::from_millis(1),
            budget,
        );
        assert!(controller.downsize() < coarse);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
    }

    #[test]
    fn sustained_fill_cost_can_coarsen_beyond_eight_and_recovers_on_the_first_fast_frame() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed_pixel_cost(&mut controller, 32, 2.5);
        assert!(controller.downsize() > 8.0);
        let coarse = controller.downsize();
        feed(&mut controller, 1, 1.0 / 60.0, 0.004);
        assert!(
            controller.downsize() < coarse,
            "a cheap frame restores detail immediately"
        );
        feed(&mut controller, 10, 1.0 / 60.0, 0.004);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
    }

    #[test]
    fn achievable_pacing_and_idle_gaps_preserve_quality_while_hitches_react_immediately() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        for _ in 0..100 {
            controller.record(
                Duration::from_secs_f64(1.0 / 30.0),
                Duration::from_millis(3),
                Duration::from_secs_f64(1.0 / 30.0),
            );
        }
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
        controller.record(
            Duration::from_secs(20),
            Duration::from_millis(3),
            Duration::from_secs_f64(1.0 / 60.0),
        );
        feed(&mut controller, 100, 1.0 / 60.0, 0.003);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
        feed(&mut controller, 1, 0.10, 0.10);
        assert!(
            controller.downsize() > BASE_DOWNSIZE,
            "even the first costly frame reacts"
        );
        feed(&mut controller, 1, 1.0 / 60.0, 0.003);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
    }

    #[test]
    fn delayed_asynchronous_gpu_frames_adapt_with_little_redraw_work() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed(&mut controller, 1, 1.0 / 30.0, 0.003);
        assert!(
            controller.downsize() > BASE_DOWNSIZE,
            "first sustained-looking late interval reacts"
        );
        feed(&mut controller, 3, 1.0 / 30.0, 0.003);
        assert_eq!(
            controller.downsize(),
            BASE_DOWNSIZE,
            "unchanged presentation cost rolls back unproductive cuts"
        );
    }

    #[test]
    fn sustained_long_gpu_backpressure_adapts_and_explicit_idle_reset_breaks_it() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        for _ in 0..20 {
            let downsize = f64::from(controller.downsize());
            let cost = 0.002 + 32.0 / (downsize * downsize);
            feed(&mut controller, 1, cost, 0.003);
        }
        assert!(controller.downsize() > 8.0);

        let coarse = controller.downsize();
        controller.reset_timing();
        feed(&mut controller, 1, 20.0, 0.003);
        assert_eq!(
            controller.downsize(),
            coarse,
            "the idle gap alone is excluded"
        );
        feed(&mut controller, 1, 1.0 / 60.0, 0.003);
        assert!(
            controller.downsize() < coarse,
            "the first real cheap frame restores detail"
        );
    }

    #[test]
    fn expensive_sweeps_and_subsequent_cheap_frames_each_adapt_immediately() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed(&mut controller, 1, 0.026, 0.026);
        assert!(controller.downsize() > BASE_DOWNSIZE);
        assert!(
            controller.downsize() < 4.0,
            "bounded cut follows the costly frame"
        );
        feed(&mut controller, 1, 1.0 / 60.0, 0.005);
        assert_eq!(
            controller.downsize(),
            BASE_DOWNSIZE,
            "first cheap frame restores the baseline"
        );

        feed(&mut controller, 20, 1.0 / 60.0, 0.005);
        feed(&mut controller, 1, 0.026, 0.026);
        assert!(
            controller.downsize() > BASE_DOWNSIZE,
            "a new isolated sweep still reacts"
        );
        let after_sweep = controller.downsize();
        feed(&mut controller, 1, 1.0 / 60.0, 0.005);
        assert!(
            controller.downsize() < after_sweep,
            "the next cheap frame restores detail immediately"
        );
        feed(&mut controller, 1, 1.0 / 60.0, 0.005);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
    }

    #[test]
    fn overload_is_bounded_only_by_nonzero_target_and_can_recover_from_it() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([64, 32]);
        let mut smallest = [64, 32];
        for _ in 0..400 {
            feed_pixel_cost(&mut controller, 1, 500.0);
            let rendered = controller.extent([64, 32]);
            if rendered[0] * rendered[1] < smallest[0] * smallest[1] {
                smallest = rendered;
            }
        }
        assert_eq!(
            smallest,
            [1, 1],
            "useful cuts can still reach a one-pixel target"
        );
        feed(&mut controller, 120, 1.0 / 60.0, 0.004);
        assert!(controller.downsize() < 64.0);
    }

    #[test]
    fn fixed_cost_floor_rolls_back_unproductive_probes_and_heavier_work_reopens_them() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        let target = Duration::from_secs_f64(1.0 / 240.0);
        let mut coarsest = BASE_DOWNSIZE;
        for _ in 0..600 {
            controller.record(Duration::from_millis(7), Duration::from_millis(7), target);
            coarsest = coarsest.max(controller.downsize());
        }
        assert!(
            coarsest > BASE_DOWNSIZE,
            "controller must probe before diagnosing the floor"
        );
        assert_eq!(
            controller.downsize(),
            BASE_DOWNSIZE,
            "fixed cost must not ruin quality indefinitely"
        );
        assert!(controller.held_cost.is_some());
        controller.record(Duration::from_millis(16), Duration::from_millis(16), target);
        assert!(
            controller.downsize() > BASE_DOWNSIZE,
            "the first materially heavier frame reopens probing"
        );
    }

    #[test]
    fn fixed_overhead_improvement_recovers_from_a_tiny_held_surface() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        let target = Duration::from_secs_f64(1.0 / 240.0);

        // Warmup or presentation overhead improves independently of ribbon
        // quality. Observing that improvement after each cut cannot establish
        // that reducing the model target caused it.
        for stage in 0..8 {
            let interval = Duration::from_secs_f64(0.100 * 0.85_f64.powi(stage));
            for _ in 0..4 {
                controller.record(interval, Duration::from_millis(3), target);
            }
        }
        for _ in 0..12 {
            controller.record(Duration::from_millis(32), Duration::from_millis(3), target);
        }
        let tiny = controller.extent([5120, 2880]);
        let held_downsize = controller.downsize();

        // Fixed presentation cost then improves substantially, but remains
        // above the requested 240 Hz budget. Quality must still be allowed to
        // recover instead of requiring an unattainable cadence first.
        for _ in 0..1200 {
            controller.record(Duration::from_millis(20), Duration::from_millis(3), target);
        }
        assert_eq!(
            controller.downsize(),
            BASE_DOWNSIZE,
            "fixed presentation cost must allow detail recovery: before={held_downsize}, initial_extent={tiny:?}, final_extent={:?}",
            controller.extent([5120, 2880]),
        );
    }

    #[test]
    fn healthy_frames_restore_baseline_within_sixteen_frames_after_extreme_fill() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed_pixel_cost(&mut controller, 400, 500.0);

        // A fast zoom ends. The same steady cadence can fit finer ribbons now;
        // recovery should not take minutes just because the old scale was huge.
        feed(&mut controller, 16, 1.0 / 60.0, 0.004);
        assert_eq!(
            controller.downsize(),
            BASE_DOWNSIZE,
            "sixteen inexpensive frames should restore normal detail, extent={:?}",
            controller.extent([5120, 2880]),
        );
    }

    #[test]
    fn balanced_present_jitter_with_fast_redraw_work_preserves_quality() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        let target = Duration::from_secs_f64(1.0 / 60.0);
        let mut coarsest = BASE_DOWNSIZE;
        for frame in 0..600 {
            // The limiter/display alternates early/late delivery at 60 FPS.
            // Actual redraw work still comfortably fits the frame budget.
            let interval = if frame % 2 == 0 { 0.013333334 } else { 0.020 };
            controller.record(
                Duration::from_secs_f64(interval),
                Duration::from_millis(5),
                target,
            );
            coarsest = coarsest.max(controller.downsize());
        }
        assert_eq!(
            coarsest, BASE_DOWNSIZE,
            "balanced pacing jitter is not a rendering overload"
        );
    }

    #[test]
    fn first_near_deadline_frame_reduces_resolution_immediately() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        let initial_extent = controller.extent([5120, 2880]);
        let initial_downsize = controller.downsize();
        let budget = Duration::from_secs_f64(1.0 / 60.0);

        // The frame still meets 60 Hz, but has already consumed almost 90% of
        // its budget. Waiting for several misses leaves no safety margin.
        controller.record(budget, Duration::from_secs_f64(0.0148), budget);

        assert!(
            controller.downsize() > initial_downsize,
            "the first near-deadline frame must cut quality for the next frame: before={initial_downsize}, after={}",
            controller.downsize(),
        );
        let next_extent = controller.extent([5120, 2880]);
        assert!(next_extent[0] < initial_extent[0] && next_extent[1] < initial_extent[1]);
    }

    #[test]
    fn rising_render_cost_cuts_before_the_next_frame_reaches_the_margin() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed(&mut controller, 1, 1.0 / 60.0, 0.010);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
        // Still inside this frame's band, but the observed rise predicts 16 ms
        // next frame. Cut before that rising workload reaches the deadline.
        feed(&mut controller, 1, 1.0 / 60.0, 0.013);
        assert!(controller.downsize() > BASE_DOWNSIZE);
    }

    #[test]
    fn expected_cost_increase_from_finer_quality_is_not_extrapolated() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed_pixel_cost(&mut controller, 32, 2.5);
        feed(&mut controller, 1, 1.0 / 60.0, 0.008);
        let finer = controller.downsize();
        // The larger target itself explains this increase, which still fits
        // the hysteresis band. It must not be forecast as growing scene cost.
        feed(&mut controller, 1, 1.0 / 60.0, 0.013);
        assert_eq!(controller.downsize(), finer);
    }

    #[test]
    fn first_fast_frame_restores_resolution_immediately() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed_pixel_cost(&mut controller, 32, 2.5);
        let coarse = controller.downsize();
        assert!(
            coarse > BASE_DOWNSIZE,
            "fixture must have real fill-driven coarsening"
        );
        let budget = Duration::from_secs_f64(1.0 / 60.0);

        // A zoom ends. VSync still paces at 60 Hz, while measured redraw work
        // now has ample headroom. The very next target should gain detail.
        controller.record(budget, Duration::from_millis(4), budget);

        assert!(
            controller.downsize() < coarse,
            "the first inexpensive frame must restore detail for the next frame: before={coarse}, after={}",
            controller.downsize(),
        );
    }

    #[test]
    fn fast_redraws_never_coarsen_for_balanced_present_jitter() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        let budget = Duration::from_secs_f64(1.0 / 60.0);

        for interval in [0.016666667, 0.013333334, 0.020]
            .into_iter()
            .cycle()
            .take(300)
        {
            controller.record(
                Duration::from_secs_f64(interval),
                Duration::from_millis(4),
                budget,
            );
            assert_eq!(
                controller.downsize(),
                BASE_DOWNSIZE,
                "presentation pacing must not override measured render headroom",
            );
        }
    }

    #[test]
    fn a_fast_frame_restores_quality_after_previous_asynchronous_gpu_pressure() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed(&mut controller, 1, 1.0 / 30.0, 0.003);
        let coarse = controller.downsize();
        assert!(coarse > BASE_DOWNSIZE);

        // The current interval is healthy. The old late interval must not keep
        // supplying pressure after the current render becomes inexpensive.
        feed(&mut controller, 1, 1.0 / 60.0, 0.004);
        assert!(controller.downsize() < coarse);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
    }

    #[test]
    fn an_unsafe_finer_trial_rolls_back_then_immediately_tries_a_smaller_step() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed_pixel_cost(&mut controller, 32, 2.5);
        let safe = controller.downsize();
        assert!(safe > 8.0);

        // This workload has an extra pass once the target gets sufficiently
        // fine. Its cost is genuinely resolution-dependent, but a smooth
        // pixel-cost estimate cannot predict that additional pass beforehand.
        let render_cost = |downsize: f32| {
            let relative_pixels = f64::from(safe / downsize).powi(2);
            0.004 + 0.004 * relative_pixels + if downsize < safe * 0.85 { 0.012 } else { 0.0 }
        };
        let cost = render_cost(controller.downsize());
        feed(&mut controller, 1, cost.max(1.0 / 60.0), cost);
        assert!(
            controller.downsize() < safe,
            "headroom starts a finer trial immediately"
        );

        let failed_trial = controller.downsize();
        let cost = render_cost(failed_trial);
        assert!(
            cost > 1.0 / 60.0,
            "fixture must make that finer trial unsafe"
        );
        feed(&mut controller, 1, cost, cost);
        assert_eq!(
            controller.downsize(),
            safe,
            "one expensive trial rolls back immediately"
        );

        // The next coarse frame still has headroom. The rejected large jump
        // must limit the next step, rather than veto every finer resolution.
        let cost = render_cost(controller.downsize());
        feed(&mut controller, 1, cost.max(1.0 / 60.0), cost);
        let smaller_trial = controller.downsize();
        assert!(
            smaller_trial < safe && smaller_trial > failed_trial,
            "the next fast frame must immediately try a smaller quality increase: safe={safe}, rejected={failed_trial}, next={smaller_trial}",
        );
        let cost = render_cost(smaller_trial);
        assert!(cost < 1.0 / 60.0, "the smaller step fits this workload");
        feed(&mut controller, 1, cost.max(1.0 / 60.0), cost);
        assert!(
            controller.downsize() <= smaller_trial,
            "a safe finer target is retained"
        );
    }

    #[test]
    fn fresh_deadline_pressure_is_not_blocked_by_an_old_fixed_cost_hold() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed(&mut controller, 64, 0.020, 0.020);
        assert_eq!(
            controller.downsize(),
            BASE_DOWNSIZE,
            "a fixed cost floor restores useful quality"
        );

        feed(&mut controller, 1, 1.0 / 60.0, 0.004);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
        feed(&mut controller, 1, 1.0 / 60.0, 0.0148);
        assert!(
            controller.downsize() > BASE_DOWNSIZE,
            "new near-deadline work must cut quality on that frame"
        );
    }

    #[test]
    fn a_rising_cost_inside_a_learned_near_deadline_floor_cuts_before_the_deadline() {
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        feed(&mut controller, 64, 1.0 / 60.0, 0.0148);
        assert_eq!(
            controller.downsize(),
            BASE_DOWNSIZE,
            "unchanged fixed cost learns a floor"
        );

        // This increase is only 8% over the learned floor, but consumes most
        // of the remaining 60 Hz headroom. A generic 25% hold-release rule
        // would wait until a deadline is already missed.
        feed(&mut controller, 1, 1.0 / 60.0, 0.016);
        assert!(
            controller.downsize() > BASE_DOWNSIZE,
            "a learned near-budget floor must not suppress an immediate pre-deadline cut",
        );
    }
}
