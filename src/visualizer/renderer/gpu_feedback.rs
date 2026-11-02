//! Match delayed GPU measurements and budget the scalable model pass.
use std::time::Duration;

use crate::render::GpuFrameTiming;

use super::adaptive_resolution::BASE_DOWNSIZE;

#[derive(Default)]
pub(super) struct GpuFeedback {
    generation: u64,
    physical_viewport: Option<[u32; 2]>,
    anchor: Option<(f32, f64)>,
    limit: Option<(f32, f64)>,
}

impl GpuFeedback {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn restart(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.anchor = None;
        self.limit = None;
    }

    /// Return whether a physical resize invalidated the previous session.
    pub fn observe_viewport(&mut self, viewport: [u32; 2]) -> bool {
        if self.physical_viewport == Some(viewport) {
            return false;
        }
        self.physical_viewport = Some(viewport);
        self.restart();
        true
    }

    pub fn same_session(&self, timing: &GpuFrameTiming) -> bool {
        let tag = &timing.tag;
        self.physical_viewport == Some(tag.viewport)
            && !tag.viewport.contains(&0)
            && tag.generation == self.generation
            && !timing.elapsed.is_zero()
            && tag.downsize.is_finite()
            && tag.downsize >= 1.0
    }

    pub fn matches(&self, timing: &GpuFrameTiming, current_downsize: f32) -> bool {
        let tag = &timing.tag;
        self.same_session(timing)
            && current_downsize.is_finite()
            && current_downsize >= 1.0
            && (tag.downsize - current_downsize).abs() <= tag.downsize.max(current_downsize) * 1e-5
            && tag
                .viewport
                .map(|v| ((v as f32 / tag.downsize).ceil() as u32).max(1))
                == tag
                    .viewport
                    .map(|v| ((v as f32 / current_downsize).ceil() as u32).max(1))
    }

    /// Learn whether lowering resolution actually accelerates model drawing.
    /// Older-quality samples can still refine this evidence within one session.
    pub fn scaling_limit(&mut self, timing: &GpuFrameTiming) -> Option<f32> {
        if !self.same_session(timing) {
            return None;
        }
        let downsize = timing.tag.downsize;
        let model = timing.model_elapsed.as_secs_f64();
        if model == 0.0 {
            self.anchor = Some((downsize, model));
            self.limit = Some((BASE_DOWNSIZE, model));
            return Some(BASE_DOWNSIZE);
        }
        if self.limit.is_some_and(|(limit, cost)| {
            (downsize - limit).abs() <= downsize.max(limit) * 1e-5 && model > cost * 1.25
        }) {
            self.anchor = Some((downsize, model));
            self.limit = None;
            return None;
        }
        let Some((anchor_downsize, anchor_model)) = self.anchor else {
            self.anchor = Some((downsize, model));
            return None;
        };
        if (downsize - anchor_downsize).abs() <= downsize.max(anchor_downsize) * 1e-5
            && model > anchor_model * 1.25
        {
            // A genuinely heavier scene needs a new reference immediately,
            // even before an insensitive-scaling limit has been learned.
            self.anchor = Some((downsize, model));
            self.limit = None;
            return None;
        }
        if downsize < anchor_downsize {
            self.anchor = Some((downsize, model));
            self.limit = None;
            return None;
        }
        if (downsize - anchor_downsize).abs() <= downsize.max(anchor_downsize) * 1e-5 {
            self.anchor = Some((anchor_downsize, anchor_model.min(model)));
            if let Some((limit, cost)) = self.limit.as_mut() {
                if (*limit - anchor_downsize).abs() <= anchor_downsize.max(*limit) * 1e-5 {
                    *cost = cost.min(model);
                }
            }
        } else if downsize >= anchor_downsize * 2.0 {
            // Four times fewer target pixels should produce a measurable gain.
            // Use integer ratio coefficients to include exactly 30% improvement.
            if model * 10.0 <= anchor_model * 7.0 {
                self.anchor = Some((downsize, model));
                self.limit = None;
            } else {
                self.limit = Some((anchor_downsize, anchor_model));
            }
        }
        self.limit.map(|(limit, _)| limit)
    }

    /// Express model cost in the controller's unchanged frame-budget units.
    /// Full-resolution composition cannot be accelerated by shrinking ribbons.
    /// The caller validates the sample's tag with `matches` before using it.
    pub fn control_cost(&self, timing: &GpuFrameTiming, budget: Duration) -> Option<Duration> {
        if budget.is_zero() || timing.elapsed.is_zero() {
            return None;
        }
        let aim = budget.as_secs_f64() * 0.75;
        let fixed = timing
            .elapsed
            .saturating_sub(timing.model_elapsed)
            .as_secs_f64();
        // Keep a useful model allowance even when fixed work already exceeds
        // the requested deadline. Otherwise a cheap model chases an impossible
        // total-frame target all the way to one pixel.
        let desired_model = (aim - fixed).max(fixed * 0.25).max(f64::EPSILON);
        let normalized = timing.model_elapsed.as_secs_f64() * (aim / desired_model);
        // Budget allocation can discount an irreducible fixed cost, but must
        // never inflate a frame already below the deadline into an overload.
        // For overlapping spans, keep the larger model measurement as the
        // conservative bound rather than truncating real measured work.
        let normalized = normalized.min(timing.elapsed.max(timing.model_elapsed).as_secs_f64());
        // Real multi-second GPU work remains pressure. Saturation is only at
        // Duration's representable maximum, not an artificial timing cutoff.
        Some(Duration::try_from_secs_f64(normalized).unwrap_or(Duration::MAX))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::render::GpuFrameTag;

    fn sample(feedback: &GpuFeedback, downsize: f32, viewport: [u32; 2]) -> GpuFrameTiming {
        GpuFrameTiming {
            tag: GpuFrameTag {
                downsize,
                viewport,
                generation: feedback.generation(),
            },
            elapsed: Duration::from_millis(3),
            model_elapsed: Duration::from_millis(1),
        }
    }

    fn model_sample(model_ms: f64, fixed_ms: f64) -> GpuFrameTiming {
        GpuFrameTiming {
            tag: GpuFrameTag {
                downsize: 2.0,
                viewport: [5120, 2880],
                generation: 0,
            },
            elapsed: Duration::from_secs_f64((model_ms + fixed_ms) / 1000.0),
            model_elapsed: Duration::from_secs_f64(model_ms / 1000.0),
        }
    }

    fn scaling_sample(feedback: &GpuFeedback, downsize: f32, model_ms: f64) -> GpuFrameTiming {
        let mut measured = sample(feedback, downsize, [5120, 2880]);
        measured.elapsed = Duration::from_secs_f64((model_ms + 0.5) / 1000.0);
        measured.model_elapsed = Duration::from_secs_f64(model_ms / 1000.0);
        measured
    }

    #[test]
    fn same_quality_gpu_cost_is_accepted_without_presentation_cadence() {
        let mut feedback = GpuFeedback::default();
        let viewport = [5120, 2880];
        assert!(!feedback.matches(&sample(&feedback, 2.0, viewport), 2.0));
        assert!(feedback.observe_viewport(viewport));
        let measured = sample(&feedback, 2.0, viewport);
        // The sample is GPU execution cost only. A host presenter capped at
        // 120 Hz cannot turn this 3 ms result into an 8.33 ms render cost.
        assert!(feedback.matches(&measured, 2.0));
        assert!(measured.elapsed < Duration::from_secs_f64(1.0 / 240.0));
        assert!(!feedback.observe_viewport(viewport));
        assert!(feedback.matches(&measured, 2.0));
    }

    #[test]
    fn delayed_samples_from_an_older_quality_cannot_drive_successive_cuts() {
        let mut feedback = GpuFeedback::default();
        let viewport = [5120, 2880];
        feedback.observe_viewport(viewport);
        let measured = sample(&feedback, 2.0, viewport);
        let generation = feedback.generation();
        assert!(feedback.matches(&measured, 2.0));
        assert!(!feedback.matches(&measured, 4.0));
        assert!(
            feedback.same_session(&measured),
            "older quality remains valid scaling evidence"
        );
        assert!(!feedback.matches(&measured, 8.0));
        assert!(!feedback.matches(&measured, 5120.0));
        assert_eq!(feedback.generation(), generation);
        assert!(feedback.matches(&sample(&feedback, 4.0, viewport), 4.0));
    }

    #[test]
    fn restart_and_resize_invalidate_pending_measurements() {
        let mut feedback = GpuFeedback::default();
        let viewport = [5120, 2880];
        feedback.observe_viewport(viewport);
        let before_restart = sample(&feedback, 2.0, viewport);
        feedback.restart();
        assert!(!feedback.matches(&before_restart, 2.0));
        let after_restart = sample(&feedback, 2.0, viewport);
        assert!(feedback.matches(&after_restart, 2.0));

        let resized = [3840, 2160];
        assert!(feedback.observe_viewport(resized));
        assert!(!feedback.matches(&after_restart, 2.0));
        assert!(!feedback.matches(&sample(&feedback, 2.0, viewport), 2.0));
        assert!(feedback.matches(&sample(&feedback, 2.0, resized), 2.0));
    }

    #[test]
    fn invalid_samples_are_rejected_but_real_long_gpu_work_is_valid() {
        let mut feedback = GpuFeedback::default();
        let viewport = [5120, 2880];
        feedback.observe_viewport(viewport);
        let mut measured = sample(&feedback, 2.0, viewport);
        measured.elapsed = Duration::ZERO;
        assert!(!feedback.matches(&measured, 2.0));
        measured.elapsed = Duration::from_secs(3);
        assert!(feedback.matches(&measured, 2.0));
        for downsize in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(!feedback.matches(&measured, downsize));
            assert!(!feedback.matches(&sample(&feedback, downsize, viewport), 2.0));
        }
        feedback.observe_viewport([0, 2880]);
        assert!(!feedback.matches(&sample(&feedback, 2.0, [0, 2880]), 2.0));
    }

    #[test]
    fn quality_comparison_allows_only_small_relative_rounding_error() {
        let mut feedback = GpuFeedback::default();
        let viewport = [5120, 2880];
        feedback.observe_viewport(viewport);
        let measured = sample(&feedback, 2.0, viewport);
        assert!(feedback.matches(&measured, 2.00001));
        assert!(!feedback.matches(&measured, 2.0001));
        let almost_one_pixel = sample(&feedback, 5119.99, viewport);
        assert!(
            !feedback.matches(&almost_one_pixel, 5120.0),
            "a 2x1 sample is not a 1x1 sample even when factors are close"
        );
    }

    #[test]
    fn a_cheap_model_keeps_headroom_when_fixed_gpu_work_exceeds_the_deadline() {
        let feedback = GpuFeedback::default();
        let budget = Duration::from_secs_f64(1.0 / 240.0);
        let measured = model_sample(0.3, 10.0);
        let cost = feedback.control_cost(&measured, budget).unwrap();
        assert!(measured.elapsed > budget);
        assert!(cost < budget.mul_f64(0.65));
        assert!((cost.as_secs_f64() * 1000.0 - 0.375).abs() < 0.00001);
    }

    #[test]
    fn an_expensive_model_still_uses_real_remaining_frame_headroom() {
        let feedback = GpuFeedback::default();
        let measured = model_sample(15.0, 5.0);
        let budget = Duration::from_secs_f64(1.0 / 60.0);
        let cost = feedback.control_cost(&measured, budget).unwrap();
        assert!(cost > budget.mul_f64(0.85));
        assert!((cost.as_secs_f64() - 0.020).abs() < 0.000001);

        let faster_budget = Duration::from_secs_f64(1.0 / 240.0);
        let faster_cost = feedback.control_cost(&measured, faster_budget).unwrap();
        // Fixed work alone exceeds this deadline, but the 15 ms model remains
        // meaningfully reducible relative to its 1.25 ms minimum allowance.
        assert!(faster_cost > faster_budget.mul_f64(0.85));
        assert!((faster_cost.as_secs_f64() - 0.020).abs() < 0.000001);
    }

    #[test]
    fn a_whole_gpu_frame_below_the_margin_is_not_normalized_into_overload() {
        let feedback = GpuFeedback::default();
        let budget = Duration::from_secs_f64(1.0 / 120.0);
        let measured = model_sample(2.0, 4.8);
        assert!(measured.elapsed < budget.mul_f64(0.85));
        let cost = feedback.control_cost(&measured, budget).unwrap();
        assert!(
            cost <= measured.elapsed,
            "budget allocation cannot invent additional GPU work"
        );
        assert!(cost < budget.mul_f64(0.85));
    }

    #[test]
    fn model_budget_feedback_restores_quality_without_changing_the_target() {
        use super::super::adaptive_resolution::AdaptiveResolution;

        let feedback = GpuFeedback::default();
        let budget = Duration::from_secs_f64(1.0 / 240.0);
        let mut controller = AdaptiveResolution::default();
        controller.enable(true);
        controller.extent([5120, 2880]);
        let expensive = feedback
            .control_cost(&model_sample(15.0, 5.0), budget)
            .unwrap();
        controller.record(budget, expensive, budget);
        let coarse = controller.downsize();
        assert!(coarse > BASE_DOWNSIZE);

        let cheap = feedback
            .control_cost(&model_sample(0.3, 10.0), budget)
            .unwrap();
        controller.record(budget, cheap, budget);
        assert!(controller.downsize() < coarse);
        assert_eq!(controller.downsize(), BASE_DOWNSIZE);
    }

    #[test]
    fn zero_model_cost_is_valid_and_overlapping_spans_are_conservative() {
        let feedback = GpuFeedback::default();
        let budget = Duration::from_secs_f64(1.0 / 240.0);
        assert_eq!(
            feedback.control_cost(&model_sample(0.0, 10.0), budget),
            Some(Duration::ZERO)
        );
        assert_eq!(feedback.control_cost(&model_sample(0.0, 0.0), budget), None);
        assert_eq!(
            feedback.control_cost(&model_sample(1.0, 1.0), Duration::ZERO),
            None
        );

        let mut overlapping = model_sample(12.0, 0.0);
        overlapping.elapsed = Duration::from_millis(10);
        assert_eq!(
            feedback.control_cost(&overlapping, budget),
            Some(Duration::from_millis(12))
        );
    }

    #[test]
    fn resolution_insensitive_model_cost_caps_unhelpful_cuts() {
        let mut feedback = GpuFeedback::default();
        feedback.observe_viewport([5120, 2880]);
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 2.0)),
            None
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 2.0)),
            Some(2.0)
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 2.0)),
            Some(2.0)
        );
    }

    #[test]
    fn native_resolution_is_a_valid_scaling_reference_and_limit() {
        let mut feedback = GpuFeedback::default();
        feedback.observe_viewport([5120, 2880]);
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 1.0, 2.0)),
            None
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 2.0)),
            Some(1.0)
        );
        // Actual increased pixel work reopens scaling from the native target.
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 1.0, 20.0)),
            None
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 6.0)),
            None
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 5.5)),
            Some(2.0)
        );
    }

    #[test]
    fn measured_pixel_cost_gains_allow_coarsening_until_the_gain_stops() {
        let mut feedback = GpuFeedback::default();
        feedback.observe_viewport([5120, 2880]);
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 20.0)),
            None
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 6.0)),
            None
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 8.0, 5.5)),
            Some(4.0)
        );
    }

    #[test]
    fn exact_thirty_percent_gain_and_zero_model_cost_have_explicit_behavior() {
        let mut feedback = GpuFeedback::default();
        feedback.observe_viewport([5120, 2880]);
        feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 20.0));
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 14.0)),
            None
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 8.0, 0.0)),
            Some(BASE_DOWNSIZE)
        );
    }

    #[test]
    fn heavier_work_at_a_learned_limit_reopens_scaling() {
        let mut feedback = GpuFeedback::default();
        feedback.observe_viewport([5120, 2880]);
        feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 2.0));
        feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 2.0));
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 2.5)),
            Some(2.0)
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 2.6)),
            None
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 1.0)),
            None
        );
    }

    #[test]
    fn finer_quality_clears_an_old_scaling_constraint() {
        let mut feedback = GpuFeedback::default();
        feedback.observe_viewport([5120, 2880]);
        feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 20.0));
        feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 6.0));
        feedback.scaling_limit(&scaling_sample(&feedback, 8.0, 5.5));
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 3.0, 7.0)),
            None
        );
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 6.0, 6.5)),
            Some(3.0)
        );
    }

    #[test]
    fn delayed_same_session_anchor_samples_remove_warmup_bias() {
        let mut feedback = GpuFeedback::default();
        feedback.observe_viewport([5120, 2880]);
        feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 3.0));
        let older_quality = scaling_sample(&feedback, 2.0, 2.0);
        assert!(!feedback.matches(&older_quality, 4.0));
        assert!(feedback.same_session(&older_quality));
        feedback.scaling_limit(&older_quality);
        // Compared only with the warmup spike, 1.8 ms looks like a 40% win.
        // The better q2 evidence reveals that the actual gain is only 10%.
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 1.8)),
            Some(2.0)
        );
    }

    #[test]
    fn a_new_heavy_scene_is_not_compared_to_an_old_cheap_scene() {
        let mut feedback = GpuFeedback::default();
        feedback.observe_viewport([5120, 2880]);
        feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 2.0));
        feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 20.0));
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 6.0)),
            None
        );
    }

    #[test]
    fn stale_sessions_cannot_poison_scaling_evidence() {
        let mut feedback = GpuFeedback::default();
        feedback.observe_viewport([5120, 2880]);
        feedback.scaling_limit(&scaling_sample(&feedback, 2.0, 2.0));
        let mut old = scaling_sample(&feedback, 2.0, 0.0);
        old.tag.generation = old.tag.generation.wrapping_sub(1);
        assert_eq!(feedback.scaling_limit(&old), None);
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 4.0, 0.8)),
            None
        );
        feedback.restart();
        assert_eq!(
            feedback.scaling_limit(&scaling_sample(&feedback, 8.0, 4.0)),
            None
        );
    }
}
