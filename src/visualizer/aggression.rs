//! Spring relaxation driven by the same centered spectrum shown in analysis.

use super::BAND_COUNT;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// The default four lowest frequency regions of the spectral shape graph.
pub const AGGRESSION_LF_BANDS: [usize; 4] = [0, 1, 2, 3];
/// Four mid-frequency regions with centers at approximately 600 Hz–2 kHz.
pub const AGGRESSION_MF_BANDS: [usize; 4] = [7, 8, 9, 10];

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
    /// A low-minus-mid difference of -1 is warm (0); +1 is aggressive (1).
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
            value: ((lf_level - mf_level + 1.0) * 0.5).clamp(0.0, 1.0),
        }
    }

    /// Preserve the configured stiffness for aggressive sound or zero effect.
    /// A small positive floor lets even fully relaxed springs reach their targets.
    pub fn stiffness_scale(self, relaxation: f32) -> f32 {
        (1.0 - relaxation.clamp(0.0, 1.0) * (1.0 - self.value)).clamp(0.05, 1.0)
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
    fn maps_the_requested_difference_range_and_clamps_beyond_it() {
        for (low, mid, expected) in [
            (-0.5, 0.5, 0.0),
            (0.0, 0.0, 0.5),
            (0.5, -0.5, 1.0),
            (-1.0, 1.0, 0.0),
            (1.0, -1.0, 1.0),
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
}
