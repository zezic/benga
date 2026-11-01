//! Spring relaxation driven by the same centered spectrum shown in analysis.

use super::BAND_COUNT;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// The default four lowest frequency regions of the spectral shape graph.
pub const AGGRESSION_LF_BANDS: [usize; 4] = [0, 1, 2, 3];
/// Four mid-frequency regions with centers at approximately 600 Hz–2 kHz.
pub const AGGRESSION_MF_BANDS: [usize; 4] = [7, 8, 9, 10];
/// Default falloff time constant, bridging brief LF gaps with instant attack.
pub(crate) const AGGRESSION_RELEASE_SECONDS: f32 = 3.0;
pub(crate) const LF_BOOST_GAIN_MAX: f32 = 4.0;
pub(crate) const AGGRESSION_STIFFNESS_MAX: f32 = 2.0;

/// Selected frequency regions, with inclusive starts and exclusive ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AggressionBands {
    pub lf_start: usize,
    pub lf_end: usize,
    pub mf_start: usize,
    pub mf_end: usize,
}

impl Default for AggressionBands {
    fn default() -> Self {
        Self {
            lf_start: AGGRESSION_LF_BANDS[0],
            lf_end: AGGRESSION_LF_BANDS[3] + 1,
            mf_start: AGGRESSION_MF_BANDS[0],
            mf_end: AGGRESSION_MF_BANDS[3] + 1,
        }
    }
}

impl AggressionBands {
    pub fn validate(self) -> anyhow::Result<()> {
        for (name, range) in [("LF", self.lf_range()), ("MF", self.mf_range())] {
            anyhow::ensure!(
                range.start < range.end && range.end <= BAND_COUNT,
                "aggression {name} bands must be nonempty and within 0..{BAND_COUNT}"
            );
        }
        Ok(())
    }

    pub fn lf_range(self) -> Range<usize> {
        self.lf_start..self.lf_end
    }

    pub fn mf_range(self) -> Range<usize> {
        self.mf_start..self.mf_end
    }
}

/// Low- and mid-frequency evidence, in the graph's -1 to +1 coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectralAggression {
    pub lf_level: f32,
    pub mf_level: f32,
    /// Applied fast-over-slow LF contribution after spectral weighting and gain.
    pub lf_boost: f32,
    /// Unclamped aggression, including LF boost when added. The base shape maps
    /// a low-minus-mid difference of -1 to warm (0), and +1 to aggressive (1).
    /// Spring control maps values above one directly to at most 2x stiffness.
    pub value: f32,
}

impl Default for SpectralAggression {
    fn default() -> Self {
        Self::from_shape(&[0.0; BAND_COUNT], AggressionBands::default())
    }
}

impl SpectralAggression {
    /// Read the displayed, already normalized and smoothed spectral shape.
    pub fn from_shape(shape: &[f32; BAND_COUNT], bands: AggressionBands) -> Self {
        debug_assert!(bands.validate().is_ok());
        let lf_level = shape[bands.lf_range()]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let mid_count = bands.mf_range().len();
        let mut mid = [0.0; BAND_COUNT];
        let mid = &mut mid[..mid_count];
        mid.copy_from_slice(&shape[bands.mf_range()]);
        mid.sort_unstable_by(f32::total_cmp);
        let mf_level = if mid_count % 2 == 0 {
            (mid[mid_count / 2 - 1] + mid[mid_count / 2]) * 0.5
        } else {
            mid[mid_count / 2]
        };
        Self {
            lf_level,
            mf_level,
            lf_boost: 0.0,
            value: (lf_level - mf_level + 1.0) * 0.5,
        }
    }

    /// Attenuate base aggression before adding LF evidence. Measured band levels
    /// stay unchanged so they continue to determine the LF boost's weighting.
    pub fn with_spectrum_amount(mut self, amount: f32) -> Self {
        self.value *= amount.clamp(0.0, 1.0);
        self
    }

    /// Add unthresholded LF evidence, attenuated by the spectral LF/MF balance.
    /// A difference of -1 or below mutes the boost; 0 or above applies it fully.
    pub fn with_lf_boost(self, lf_boost: f32) -> Self {
        self.with_lf_boost_gain(lf_boost, 1.0)
    }

    /// Amplify the weighted LF evidence without capping its contribution or sum.
    pub fn with_lf_boost_gain(mut self, lf_boost: f32, gain: f32) -> Self {
        let weight = (self.lf_level - self.mf_level + 1.0).clamp(0.0, 1.0);
        self.lf_boost = lf_boost.clamp(0.0, 1.0) * weight * gain.clamp(0.0, LF_BOOST_GAIN_MAX);
        self.value += self.lf_boost;
        self
    }

    /// Relax warm sound, then apply aggression above one directly as overdrive.
    /// The positive floor and 2x ceiling bound spring control, not the envelope.
    pub fn stiffness_scale(self, relaxation: f32) -> f32 {
        let scale = if self.value > 1.0 {
            self.value
        } else {
            1.0 - relaxation.clamp(0.0, 1.0) * (1.0 - self.value.max(0.0))
        };
        scale.clamp(0.05, AGGRESSION_STIFFNESS_MAX)
    }
}

/// Audio-clock envelope for the aggression value driving spring stiffness.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AggressionEnvelope {
    value: Option<f32>,
}

impl AggressionEnvelope {
    pub(crate) fn value(&self) -> f32 {
        self.value
            .unwrap_or_else(|| SpectralAggression::default().value)
    }

    #[cfg(test)]
    pub(crate) fn observe(&mut self, target: f32, dt: f32) -> f32 {
        self.observe_with_release(target, dt, AGGRESSION_RELEASE_SECONDS)
    }

    /// The first measurement initializes directly; later rises snap to target.
    /// Falling measurements use the chosen exponential time constant, with zero
    /// selecting immediate release. Values remain unclamped throughout.
    pub(crate) fn observe_with_release(
        &mut self,
        target: f32,
        dt: f32,
        release_seconds: f32,
    ) -> f32 {
        let value = match self.value {
            Some(current) if target < current && release_seconds > 0.0 => {
                target + (current - target) * (-dt.max(0.0) / release_seconds).exp()
            }
            _ => target,
        };
        self.value = Some(value);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape_with_levels(low: f32, mid: f32) -> [f32; BAND_COUNT] {
        let mut shape = [0.0; BAND_COUNT];
        for band in AGGRESSION_LF_BANDS {
            shape[band] = low;
        }
        for band in AGGRESSION_MF_BANDS {
            shape[band] = mid;
        }
        shape
    }

    #[test]
    fn maps_the_requested_difference_range_without_clamping_beyond_it() {
        for (low, mid, expected) in [
            (-0.5, 0.5, 0.0),
            (0.0, 0.0, 0.5),
            (0.5, -0.5, 1.0),
            (-1.0, 1.0, -0.5),
            (1.0, -1.0, 1.5),
            (0.25, -0.25, 0.75),
        ] {
            let result = SpectralAggression::from_shape(
                &shape_with_levels(low, mid),
                AggressionBands::default(),
            );
            assert_eq!(result.lf_level, low);
            assert_eq!(result.mf_level, mid);
            assert_eq!(result.value, expected);
        }
        assert_eq!(SpectralAggression::default().value, 0.5);
    }

    #[test]
    fn uses_the_low_maximum_and_average_of_the_middle_two_mid_bands() {
        let mut shape = [0.0; BAND_COUNT];
        for (band, value) in AGGRESSION_LF_BANDS.into_iter().zip([-0.7, -0.3, 0.5, 0.1]) {
            shape[band] = value;
        }
        for (band, value) in AGGRESSION_MF_BANDS.into_iter().zip([1.0, -0.5, -1.0, 0.0]) {
            shape[band] = value;
        }
        let result = SpectralAggression::from_shape(&shape, AggressionBands::default());
        assert_eq!(result.lf_level, 0.5);
        assert_eq!(result.mf_level, -0.25);
        assert_eq!(result.value, 0.875);
    }

    #[test]
    fn other_bands_do_not_contribute() {
        let mut shape = shape_with_levels(-0.25, 0.25);
        let expected = SpectralAggression::from_shape(&shape, AggressionBands::default());
        for (band, level) in shape.iter_mut().enumerate() {
            if !AGGRESSION_LF_BANDS.contains(&band) && !AGGRESSION_MF_BANDS.contains(&band) {
                *level = if band % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
        assert_eq!(
            SpectralAggression::from_shape(&shape, AggressionBands::default()),
            expected
        );
    }

    #[test]
    fn relaxation_scales_stiffness_without_freezing_a_spring() {
        let warm = SpectralAggression::from_shape(
            &shape_with_levels(-0.5, 0.5),
            AggressionBands::default(),
        );
        let aggressive = SpectralAggression::from_shape(
            &shape_with_levels(0.5, -0.5),
            AggressionBands::default(),
        );
        assert_eq!(warm.stiffness_scale(0.0), 1.0);
        assert_eq!(warm.stiffness_scale(0.75), 0.25);
        assert_eq!(warm.stiffness_scale(1.0), 0.05);
        assert_eq!(SpectralAggression::default().stiffness_scale(0.75), 0.625);
        assert_eq!(aggressive.stiffness_scale(1.0), 1.0);
    }

    #[test]
    fn aggression_above_one_drives_stiffness_directly_up_to_two_times() {
        for relaxation in [0.0, 0.5, 0.75, 1.0] {
            for aggression in [1.0, 1.25, 1.5, 2.0, 5.5] {
                let state = SpectralAggression {
                    value: aggression,
                    ..SpectralAggression::default()
                };
                assert_eq!(state.stiffness_scale(relaxation), aggression.min(2.0));
                assert_eq!(
                    state.value, aggression,
                    "spring control preserves the envelope"
                );
            }
            for aggression in [0.9999, 1.0001] {
                let state = SpectralAggression {
                    value: aggression,
                    ..SpectralAggression::default()
                };
                assert!((state.stiffness_scale(relaxation) - 1.0).abs() <= 0.00011);
            }
        }
    }

    #[test]
    fn custom_ranges_choose_their_own_maximum_and_odd_median() {
        let mut shape = [1.0; BAND_COUNT];
        shape[3..5].copy_from_slice(&[-0.5, 0.25]);
        shape[11..14].copy_from_slice(&[0.5, -0.25, -0.75]);
        let bands = AggressionBands {
            lf_start: 3,
            lf_end: 5,
            mf_start: 11,
            mf_end: 14,
        };
        let result = SpectralAggression::from_shape(&shape, bands);
        assert_eq!(result.lf_level, 0.25);
        assert_eq!(result.mf_level, -0.25);
        assert_eq!(result.value, 0.75);
    }

    #[test]
    fn a_single_band_and_overlapping_ranges_are_valid() {
        let mut shape = [0.0; BAND_COUNT];
        shape[15] = -0.75;
        let bands = AggressionBands {
            lf_start: 15,
            lf_end: 16,
            mf_start: 15,
            mf_end: 16,
        };
        assert!(bands.validate().is_ok());
        let result = SpectralAggression::from_shape(&shape, bands);
        assert_eq!(result.lf_level, -0.75);
        assert_eq!(result.mf_level, -0.75);
        assert_eq!(result.value, 0.5);
    }

    #[test]
    fn rejects_empty_reversed_and_out_of_bounds_ranges() {
        assert!(AggressionBands::default().validate().is_ok());
        for (start, end) in [
            (0, 0),
            (5, 4),
            (0, BAND_COUNT + 1),
            (BAND_COUNT, BAND_COUNT + 1),
        ] {
            let low = AggressionBands {
                lf_start: start,
                lf_end: end,
                ..AggressionBands::default()
            };
            let mid = AggressionBands {
                mf_start: start,
                mf_end: end,
                ..AggressionBands::default()
            };
            assert!(low.validate().is_err());
            assert!(mid.validate().is_err());
        }
    }

    #[test]
    fn envelope_initializes_from_the_first_measurement_and_rises_immediately() {
        let mut envelope = AggressionEnvelope::default();
        assert_eq!(envelope.value(), 0.5);
        assert_eq!(envelope.observe(0.1, 1.0 / 240.0), 0.1);
        assert_eq!(envelope.observe(1.0, 1.0 / 240.0), 1.0);
        assert!(envelope.observe(0.0, 1.0) < 1.0);
        assert_eq!(envelope.observe(0.9, 0.0), 0.9);
        assert_eq!(envelope.value(), 0.9);
    }

    #[test]
    fn spectrum_amount_scales_only_the_base_before_the_lf_addition() {
        let shape = SpectralAggression::from_shape(
            &shape_with_levels(-0.25, 0.25),
            AggressionBands::default(),
        );
        for amount in [0.0, 0.5, 1.0] {
            let mixed = shape
                .with_spectrum_amount(amount)
                .with_lf_boost_gain(0.5, 2.0);
            assert_eq!(mixed.lf_boost, 0.5);
            assert_eq!(mixed.value, shape.value * amount + 0.5);
            assert_eq!(mixed.lf_level, shape.lf_level);
            assert_eq!(mixed.mf_level, shape.mf_level);
        }
    }

    #[test]
    fn spectrum_amount_preserves_the_lf_gate_and_signed_base_values() {
        let warm = SpectralAggression::from_shape(
            &shape_with_levels(-1.0, 1.0),
            AggressionBands::default(),
        );
        assert_eq!(warm.with_spectrum_amount(0.5).value, -0.25);
        let envelope_only = warm
            .with_spectrum_amount(0.0)
            .with_lf_boost_gain(1.0, LF_BOOST_GAIN_MAX);
        assert_eq!(envelope_only.value, 0.0);
        assert_eq!(envelope_only.lf_boost, 0.0);
        assert_eq!(envelope_only.lf_level, warm.lf_level);
        assert_eq!(envelope_only.mf_level, warm.mf_level);
        let aggressive = SpectralAggression::from_shape(
            &shape_with_levels(1.0, -1.0),
            AggressionBands::default(),
        );
        assert_eq!(aggressive.with_spectrum_amount(1.0).value, 1.5);
        assert_eq!(aggressive.with_spectrum_amount(0.75).value, 1.125);
    }

    #[test]
    fn lf_boost_adds_to_shape_aggression_without_changing_band_levels() {
        let shape = SpectralAggression::from_shape(
            &shape_with_levels(-0.25, 0.25),
            AggressionBands::default(),
        );
        let boosted = shape.with_lf_boost(0.5);
        assert_eq!(boosted.value, 0.5);
        assert_eq!(boosted.lf_boost, 0.25);
        assert_eq!(boosted.lf_level, shape.lf_level);
        assert_eq!(boosted.mf_level, shape.mf_level);
        assert_eq!(shape.with_lf_boost(-1.0), shape);
        let saturated = shape.with_lf_boost(2.0);
        assert_eq!(saturated.value, 0.75);
        assert_eq!(saturated.lf_boost, 0.5);
    }

    #[test]
    fn lf_boost_fades_in_from_warm_balance_and_is_full_when_lf_matches_mf() {
        for (difference, weight) in [
            (-2.0, 0.0),
            (-1.0, 0.0),
            (-0.5, 0.5),
            (0.0, 1.0),
            (0.5, 1.0),
            (2.0, 1.0),
        ] {
            let shape = SpectralAggression::from_shape(
                &shape_with_levels(difference * 0.5, -difference * 0.5),
                AggressionBands::default(),
            );
            let boosted = shape.with_lf_boost(0.8);
            assert_eq!(boosted.lf_boost, 0.8 * weight);
            assert_eq!(boosted.lf_level, shape.lf_level);
            assert_eq!(boosted.mf_level, shape.mf_level);
            assert_eq!(boosted.value, shape.value + 0.8 * weight);
        }
    }

    #[test]
    fn lf_boost_gain_preserves_unity_and_zero_disables_only_the_addition() {
        let shape = SpectralAggression::from_shape(
            &shape_with_levels(-0.25, 0.25),
            AggressionBands::default(),
        );
        assert_eq!(shape.with_lf_boost_gain(0.5, 0.0), shape);
        assert_eq!(shape.with_lf_boost_gain(0.5, 1.0), shape.with_lf_boost(0.5));
    }

    #[test]
    fn lf_boost_gain_amplifies_weighted_evidence_without_saturation() {
        let shape = SpectralAggression::from_shape(
            &shape_with_levels(-0.25, 0.25),
            AggressionBands::default(),
        );
        for (gain, expected_boost) in [(1.0, 0.375), (2.0, 0.75), (4.0, 1.5)] {
            let amplified = shape.with_lf_boost_gain(0.75, gain);
            assert_eq!(amplified.lf_boost, expected_boost);
            assert_eq!(amplified.value, shape.value + expected_boost);
            assert_eq!(amplified.lf_level, shape.lf_level);
            assert_eq!(amplified.mf_level, shape.mf_level);
        }
        let warm = SpectralAggression::from_shape(
            &shape_with_levels(-0.5, 0.5),
            AggressionBands::default(),
        );
        assert_eq!(warm.with_lf_boost_gain(1.0, LF_BOOST_GAIN_MAX), warm);
    }

    #[test]
    fn lf_boost_attacks_immediately_and_releases_through_the_aggression_envelope() {
        let shape = SpectralAggression::from_shape(
            &shape_with_levels(-0.25, 0.25),
            AggressionBands::default(),
        );
        let mut envelope = AggressionEnvelope::default();
        envelope.observe(shape.value, 0.0);
        let boosted = shape.with_lf_boost(0.5);
        assert_eq!(envelope.observe(boosted.value, 0.005), 0.5);
        let released = envelope.observe(shape.with_lf_boost(0.0).value, 0.25);
        assert!(released > 0.475);
        assert!((released - (0.25 + 0.25 * (-0.25_f32 / 3.0).exp())).abs() < 1e-6);
    }

    #[test]
    fn envelope_bridges_short_bass_gaps_but_releases_during_sustained_warm_sound() {
        let mut envelope = AggressionEnvelope::default();
        envelope.observe(1.0, 0.0);
        assert!(envelope.observe(0.0, 0.25) > 0.9);
        let after_three_seconds = envelope.observe(0.0, 2.75);
        assert!((after_three_seconds - (-1.0_f32).exp()).abs() < 1e-6);
        assert!(envelope.observe(0.0, 6.0) < 0.05);
    }

    #[test]
    fn envelope_release_depends_on_audio_time_instead_of_hop_partitioning() {
        let mut single_hop = AggressionEnvelope::default();
        let mut many_hops = AggressionEnvelope::default();
        single_hop.observe(0.95, 0.0);
        many_hops.observe(0.95, 0.0);
        single_hop.observe(0.15, 3.0);
        for _ in 0..576 {
            many_hops.observe(0.15, 3.0 / 576.0);
        }
        assert!((single_hop.value() - many_hops.value()).abs() < 1e-5);
    }

    #[test]
    fn adjustable_falloff_changes_decay_speed_while_rise_stays_immediate() {
        let mut fast = AggressionEnvelope::default();
        let mut slow = AggressionEnvelope::default();
        fast.observe_with_release(2.0, 0.0, 0.25);
        slow.observe_with_release(2.0, 0.0, 5.0);
        let fast_value = fast.observe_with_release(-0.5, 0.5, 0.25);
        let slow_value = slow.observe_with_release(-0.5, 0.5, 5.0);
        assert!(fast_value < 0.0 && slow_value > 1.0);
        assert!((fast_value - (-0.5 + 2.5 * (-2.0_f32).exp())).abs() < 1e-6);
        assert!((slow_value - (-0.5 + 2.5 * (-0.1_f32).exp())).abs() < 1e-6);
        assert_eq!(fast.observe_with_release(3.0, 0.005, 0.25), 3.0);
        assert_eq!(slow.observe_with_release(3.0, 0.005, 5.0), 3.0);
    }

    #[test]
    fn zero_falloff_releases_immediately_even_without_elapsed_time() {
        let mut envelope = AggressionEnvelope::default();
        envelope.observe_with_release(3.0, 0.0, 10.0);
        assert_eq!(envelope.observe_with_release(-0.5, 0.0, 0.0), -0.5);
        assert_eq!(envelope.observe_with_release(2.0, 0.005, 0.0), 2.0);
    }

    #[test]
    fn changing_falloff_preserves_the_held_value_and_uses_the_new_time_constant() {
        let mut envelope = AggressionEnvelope::default();
        envelope.observe_with_release(2.0, 0.0, 10.0);
        let held = envelope.observe_with_release(-1.0, 0.1, 10.0);
        assert_eq!(envelope.observe_with_release(-1.0, 0.0, 0.5), held);
        let released = envelope.observe_with_release(-1.0, 0.5, 0.5);
        assert!((released - (-1.0 + (held + 1.0) * (-1.0_f32).exp())).abs() < 1e-6);
    }

    #[test]
    fn nondefault_falloff_is_independent_of_audio_hop_partitioning() {
        let mut single_hop = AggressionEnvelope::default();
        let mut many_hops = AggressionEnvelope::default();
        single_hop.observe_with_release(2.0, 0.0, 1.75);
        many_hops.observe_with_release(2.0, 0.0, 1.75);
        single_hop.observe_with_release(-0.5, 1.5, 1.75);
        for _ in 0..300 {
            many_hops.observe_with_release(-0.5, 0.005, 1.75);
        }
        assert!((single_hop.value() - many_hops.value()).abs() < 1e-5);
    }

    #[test]
    fn envelope_release_holds_without_elapsed_time_and_preserves_unclamped_targets() {
        let mut envelope = AggressionEnvelope::default();
        assert_eq!(envelope.observe(2.0, 0.0), 2.0);
        assert_eq!(envelope.observe(-1.0, 0.0), 2.0);
        assert_eq!(envelope.observe(-1.0, -1.0), 2.0);
        assert!((envelope.observe(1.0, 3.0) - (1.0 + (-1.0_f32).exp())).abs() < 1e-6);
        for _ in 0..100 {
            let value = envelope.observe(-1.0, 0.1);
            assert!((-1.0..=2.0).contains(&value));
        }
        let value = envelope.value();
        assert!(value < 0.0);
        assert_eq!(envelope.observe(value, 3.0), value);
    }

    #[test]
    fn an_unclamped_peak_drives_overstiffness_until_its_release_crosses_one() {
        let shape = SpectralAggression::default();
        let peak = shape.with_lf_boost_gain(0.75, 2.0);
        assert_eq!(peak.value, 2.0);
        let mut envelope = AggressionEnvelope::default();
        assert_eq!(envelope.observe(peak.value, 0.005), 2.0);
        assert_eq!(peak.stiffness_scale(0.75), 2.0);
        let after_one_second = SpectralAggression {
            value: envelope.observe(0.0, 1.0),
            ..peak
        };
        assert!((after_one_second.value - 2.0 * (-1.0_f32 / 3.0).exp()).abs() < 1e-6);
        assert!(after_one_second.value > 1.0);
        assert_eq!(
            after_one_second.stiffness_scale(0.75),
            after_one_second.value
        );
        let after_three_seconds = SpectralAggression {
            value: envelope.observe(0.0, 2.0),
            ..peak
        };
        assert!(after_three_seconds.value < 1.0);
        assert!(after_three_seconds.stiffness_scale(0.75) < 1.0);
    }

    #[test]
    fn negative_aggression_clamps_only_when_computing_the_relaxation_factor() {
        let warm = SpectralAggression::from_shape(
            &shape_with_levels(-1.0, 1.0),
            AggressionBands::default(),
        );
        assert_eq!(warm.value, -0.5);
        let mut envelope = AggressionEnvelope::default();
        assert_eq!(envelope.observe(warm.value, 0.005), -0.5);
        assert_eq!(warm.stiffness_scale(0.0), 1.0);
        assert_eq!(warm.stiffness_scale(0.5), 0.5);
        assert_eq!(warm.stiffness_scale(0.75), 0.25);
        assert_eq!(warm.stiffness_scale(1.0), 0.05);
    }
}
