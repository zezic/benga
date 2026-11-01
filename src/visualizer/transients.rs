//! Bounded causal attacks classified against paired, preceding populations.
//! No percussion decision uses the arrangement FFT or an EMA score threshold.
use std::collections::VecDeque;

use super::detection::{DetectorEvents, OnsetFluxStatus, TriggerStatus};
use super::percussion::*;
use super::percussion_spectrum::PercussionFrame;

const MAX_SEGMENTS: usize = 128;
const MAX_PENDING: usize = 8;
/// Small interpolation allowances, independent of the plotting bins.
const SHARPNESS_TOLERANCE: f32 = 0.01;
/// 3% energy is 0.128 dB.
const ENERGY_RELATIVE_TOLERANCE: f32 = 0.03;

/// Combining positive side movements here preserves mono/fixed-pan scale,
/// while a falling side cannot subtract a fresh attack on the other side.
fn stereo_rms(values: [f32; 2]) -> f32 {
    ((values[0] * values[0] + values[1] * values[1]) * 0.5).sqrt()
}

fn stereo_power_mean(values: [f32; 2]) -> f32 {
    (values[0] + values[1]) * 0.5
}

#[derive(Clone, Copy, Debug, Default)]
struct Observation {
    candidate_id: u64,
    time: f64,
    sharpness: f32,
    energy: f32,
    eligible: bool,
    provisional: bool,
    overlapping: bool,
}

struct Population {
    paired: VecDeque<Observation>,
    /// Display history survives time expiry; only retained `paired` records
    /// contribute to density, ranks and quantiles.
    recent_admissions: VecDeque<Observation>,
    sharpness: Vec<f32>,
    energy: Vec<f32>,
    sharpness_histogram: [u32; HISTOGRAM_BINS],
    energy_histogram: [u32; HISTOGRAM_BINS],
    joint_histogram: [[u32; HEATMAP_BINS]; HEATMAP_BINS],
    last_capacity_removal: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the 24 ms feature and fixed-floor recipe used by these causal fixtures.
    fn fixed_floor_channel_settings() -> PercussionChannelSettings {
        PercussionChannelSettings {
            feature_ms: 24.0,
            min_sharpness: 0.40,
            min_output_prominence: 0.16,
            ..fixed_floor_test_settings().channels[0]
        }
    }

    /// Small causal fixture runner used to settle the feature contract without
    /// percentile tuning or the frequency front end confounding the features.
    fn feature(rise: f32, gain: f32, dt: f32) -> Candidate {
        let settings = fixed_floor_channel_settings();
        let mut candidate = Candidate::new(1, 0.0, 0.0, 0.0, 0.0);
        let mut elapsed = 0.0;
        while elapsed < settings.feature_ms * 0.001 - 1e-7 {
            elapsed += dt;
            let amplitude = (elapsed / rise).min(1.0) * gain;
            candidate.push(amplitude, amplitude * amplitude, 0.25, dt, &settings);
        }
        candidate
    }

    #[test]
    fn feature_contract_decouples_concentration_gain_and_prefix_energy() {
        let fast = feature(0.002, 0.2, 0.001);
        let slow = feature(0.024, 0.2, 0.001);
        assert!((fast.concentration(0.005) - 1.0).abs() < 1e-6);
        assert!((slow.concentration(0.005) - 5.0 / 24.0).abs() < 1e-5);
        let scaled = feature(0.002, 0.4, 0.001);
        assert!((scaled.energy / fast.energy - 4.0).abs() < 1e-5);
        assert!((scaled.concentration(0.005) - fast.concentration(0.005)).abs() < 1e-6);
        let equalized = feature(0.024, 0.2 * (fast.energy / slow.energy).sqrt(), 0.001);
        assert!((equalized.energy / fast.energy - 1.0).abs() < 1e-5);
        assert!(equalized.concentration(0.005) < fast.concentration(0.005) - 0.7);
    }

    #[test]
    fn concentration_interpolates_real_time_intervals_and_censors_prefixes() {
        for dt in [0.0005, 0.0007, 0.001, 0.0013, 0.002] {
            let slow = feature(0.120, 0.5, dt);
            assert!((slow.elapsed - 0.024).abs() < 1e-7);
            assert!((slow.concentration(0.005) - 5.0 / 24.0).abs() < 1e-4);
            assert!(slow.last_growth > 0.0, "rise is still incomplete");
        }
        let negligible = feature(0.01, 1e-12, 0.001);
        assert_eq!(negligible.concentration(0.005), 0.0);
    }

    #[test]
    fn energy_integrates_hops_once_and_clips_background_crossings() {
        let settings = fixed_floor_channel_settings();
        let mut candidate = Candidate::new(1, 0.0, 0.25, 1.0, 1.0);
        for _ in 0..24 {
            candidate.push(1.0, 1.0, 1.0, 0.001, &settings);
        }
        assert!((candidate.absolute_energy - 0.024).abs() < 1e-7);
        assert!((candidate.energy - 0.018).abs() < 1e-7);
        let mut crossing = Candidate::new(1, 0.0, 0.5, 0.0, 0.0);
        crossing.push(1.0, 1.0, 1.0, 0.001, &settings);
        assert!((crossing.energy - 0.000125).abs() < 1e-9);
    }

    #[test]
    fn stereo_growth_and_energy_are_rectified_before_combining_sides() {
        let settings = fixed_floor_channel_settings();
        let build = |gain: f32| {
            let mut candidate = Candidate::new_stereo(
                1,
                0.0,
                [0.04 * gain * gain, 0.0],
                [0.2 * gain, 0.0],
                [0.04 * gain * gain, 0.0],
            );
            for _ in 0..24 {
                candidate.push_stereo(
                    [0.1 * gain, 0.03_f32.sqrt() * gain],
                    [0.01 * gain * gain, 0.03 * gain * gain],
                    0.02 * gain * gain,
                    0.001,
                    &settings,
                );
            }
            candidate
        };
        let measured = build(1.0);
        assert!((measured.absolute_energy - 0.00048).abs() < 1e-8);
        assert!((measured.energy - 0.0003525).abs() < 1e-8);
        assert!((measured.growth - 0.015_f32.sqrt()).abs() < 1e-6);
        assert!((measured.concentration(0.005) - 1.0).abs() < 1e-6);
        assert!((measured.peak_excess - measured.growth).abs() < 1e-6);
        assert!(measured.energy <= measured.absolute_energy);
        let scaled = build(2.0);
        assert!((scaled.energy / measured.energy - 4.0).abs() < 1e-5);
        assert!((scaled.growth / measured.growth - 2.0).abs() < 1e-5);
        assert!((scaled.concentration(0.005) - measured.concentration(0.005)).abs() < 1e-6);
    }

    #[test]
    fn quantiles_ties_small_populations_and_monotonic_strictness() {
        assert_eq!(quantile(&[], 100.0), None);
        assert_eq!(quantile(&[2.0], 0.0), None);
        assert_eq!(quantile(&[2.0], 1.0), Some(2.0));
        assert_eq!(quantile(&[2.0], 100.0), Some(2.0));
        assert_eq!(quantile(&[1.0, 2.0], 50.0), Some(1.0));
        assert_eq!(quantile(&[1.0, 2.0], 50.1), Some(2.0));
        let values = [0.2, 0.2, 0.4, 0.6, 0.9];
        let mut previous = 0.0;
        for percentile in 1..=100 {
            let cutoff = quantile(&values, percentile as f32).unwrap();
            assert!(cutoff >= previous);
            previous = cutoff;
        }
        assert_eq!(
            rank(&[0.4; 16], 0.4),
            RankInterval {
                lower: 0.0,
                upper: 1.0
            }
        );
        assert_eq!(
            rank(&values, 0.2),
            RankInterval {
                lower: 0.0,
                upper: 0.4
            }
        );
    }

    #[test]
    fn rejected_output_candidates_learn_and_cannot_set_their_own_cutoff() {
        let mut channel = Channel::default();
        let settings = PercussionChannelSettings {
            min_reference: 1,
            sharpness_percentile: 100.0,
            energy_percentile: 100.0,
            ..fixed_floor_channel_settings()
        };
        let first = channel.finish(feature(0.002, 0.1, 0.001), 0, 1.0, &settings);
        assert!(first.admitted && first.eligible && first.provisional);
        assert_eq!(first.reference.count, 0);
        assert_eq!(first.sharpness_rank, None);
        let second = channel.finish(feature(0.024, 0.01, 0.001), 0, 1.1, &settings);
        assert!(second.admitted && !second.eligible);
        assert_eq!(second.reference.count, 1);
        assert_eq!(second.reference.energy_cutoff, Some(first.energy));
        assert_eq!(channel.population.paired.len(), 2);
        assert_eq!(first.reference.count, 0, "saved decisions are immutable");
        assert_eq!(second.reference.energy_histogram.iter().sum::<u32>(), 1);
        let matching = channel.finish(feature(0.002, 0.1, 0.001), 0, 1.2, &settings);
        assert!(matching.eligible, "equal hits pass 100th percentile ties");
    }

    #[test]
    fn refractory_and_mix_rejections_still_update_both_histograms() {
        let mut channel = Channel::default();
        let mut settings = PercussionChannelSettings {
            min_reference: 1,
            sharpness_percentile: 0.0,
            energy_percentile: 0.0,
            ..fixed_floor_channel_settings()
        };
        assert!(
            channel
                .finish(feature(0.002, 0.1, 0.001), 0, 1.0, &settings)
                .eligible
        );
        let refractory = channel.finish(feature(0.002, 0.1, 0.001), 0, 1.01, &settings);
        assert_eq!(refractory.reason, DecisionReason::Refractory);
        assert!(refractory.admitted);
        settings.output_rms_db = 0.0;
        let quiet = channel.finish(feature(0.002, 0.1, 0.001), 0, 2.0, &settings);
        assert_eq!(quiet.reason, DecisionReason::MixRms);
        assert!(quiet.admitted);
        let snapshot = channel.population.snapshot(2.0, &settings);
        assert_eq!(snapshot.count, 3);
        assert_eq!(snapshot.sharpness_histogram.iter().sum::<u32>(), 3);
        assert_eq!(snapshot.energy_histogram.iter().sum::<u32>(), 3);
    }

    #[test]
    fn tolerance_is_reported_and_does_not_depend_on_plot_bins() {
        let mut channel = Channel::default();
        let settings = PercussionChannelSettings {
            min_reference: 1,
            sharpness_percentile: 100.0,
            energy_percentile: 100.0,
            ..fixed_floor_channel_settings()
        };
        let first = channel.finish(feature(0.002, 0.1, 0.001), 0, 1.0, &settings);
        let close = channel.finish(feature(0.002, 0.099, 0.001), 0, 2.0, &settings);
        assert!(close.eligible && close.tolerance_used);
        assert!(close.energy < first.energy);
        let rejected = channel.finish(feature(0.002, 0.09, 0.001), 0, 3.0, &settings);
        assert!(!rejected.eligible);
        assert_eq!(rejected.reason, DecisionReason::EnergyPercentile);
    }

    #[test]
    fn idle_audio_expiry_and_capacity_bounds_keep_pairs_in_lockstep() {
        let mut population = Population::default();
        let settings = PercussionChannelSettings {
            memory_seconds: 60.0,
            min_reference: 1,
            ..fixed_floor_channel_settings()
        };
        for index in 0..REFERENCE_CAPACITY + 8 {
            population.insert(
                Observation {
                    time: index as f64 * 0.001,
                    sharpness: 0.5,
                    energy: 1e-4,
                    ..Observation::default()
                },
                &settings,
            );
        }
        let snapshot = population.snapshot(9.0, &settings);
        assert_eq!(snapshot.count, REFERENCE_CAPACITY);
        assert_eq!(
            snapshot.joint_histogram.iter().flatten().sum::<u32>(),
            REFERENCE_CAPACITY as u32
        );
        assert_eq!(snapshot.recent_count, MAX_RECENT_MARKERS);
        assert!(snapshot.capacity_truncated);
        assert!(snapshot.effective_seconds < settings.memory_seconds);
        let shorter = PercussionChannelSettings {
            memory_seconds: 1.0,
            ..settings
        };
        population.expire(9.0, &shorter);
        assert!(
            !population.snapshot(9.0, &shorter).capacity_truncated,
            "shortening memory must refresh capacity diagnostics without clearing pairs"
        );
        population.expire(100.0, &settings);
        assert_eq!(population.snapshot(100.0, &settings).count, 0);
        assert!(population.sharpness.is_empty() && population.energy.is_empty());
        assert_eq!(population.sharpness_histogram.iter().sum::<u32>(), 0);
        assert_eq!(population.energy_histogram.iter().sum::<u32>(), 0);
        assert_eq!(population.joint_histogram.iter().flatten().sum::<u32>(), 0);
    }

    #[test]
    fn joint_density_marginals_and_recent_additions_survive_expiry_honestly() {
        let settings = PercussionChannelSettings {
            memory_seconds: 1.0,
            min_reference: 1,
            ..fixed_floor_channel_settings()
        };
        let mut population = Population::default();
        for index in 0..100 {
            population.insert(
                Observation {
                    candidate_id: index + 1,
                    time: index as f64 * 0.01,
                    sharpness: index as f32 / 100.0,
                    energy: (index + 1) as f32 * 1e-7,
                    eligible: index % 3 == 0,
                    provisional: index < 8,
                    overlapping: false,
                },
                &settings,
            );
        }
        let saved = population.snapshot(1.0, &settings);
        assert_eq!(
            saved.joint_histogram.iter().flatten().sum::<u32>(),
            saved.count as u32
        );
        for sharpness in 0..HEATMAP_BINS {
            assert_eq!(
                saved
                    .joint_histogram
                    .iter()
                    .map(|row| row[sharpness])
                    .sum::<u32>(),
                saved.sharpness_histogram[sharpness]
            );
        }
        for energy in 0..HEATMAP_BINS {
            assert_eq!(
                saved.joint_histogram[energy].iter().sum::<u32>(),
                saved.energy_histogram[energy]
            );
        }
        assert_eq!(saved.recent_count, MAX_RECENT_MARKERS);
        assert_eq!(saved.recent_points[0].candidate_id, 37);
        assert_eq!(
            saved.recent_points[MAX_RECENT_MARKERS - 1].candidate_id,
            100
        );
        assert!(saved.recent_points.iter().all(|point| point.in_reference));
        assert!(saved.recent_points.iter().any(|point| !point.eligible));
        population.expire(1.6, &settings);
        let expired_some = population.snapshot(1.6, &settings);
        assert_eq!(expired_some.recent_count, MAX_RECENT_MARKERS);
        assert!(expired_some
            .recent_points
            .iter()
            .any(|point| !point.in_reference));
        assert!(expired_some
            .recent_points
            .iter()
            .any(|point| point.in_reference));
        assert_eq!(
            expired_some.joint_histogram.iter().flatten().sum::<u32>(),
            expired_some.count as u32
        );
        population.expire(10.0, &settings);
        let expired_all = population.snapshot(10.0, &settings);
        assert_eq!(expired_all.count, 0);
        assert_eq!(expired_all.recent_count, MAX_RECENT_MARKERS);
        assert!(expired_all
            .recent_points
            .iter()
            .all(|point| !point.in_reference));
        assert_eq!(expired_all.joint_histogram.iter().flatten().sum::<u32>(), 0);
        assert_eq!(
            saved.count, 100,
            "saved historical density remains immutable"
        );
        assert!(saved.recent_points.iter().all(|point| point.in_reference));
    }

    #[test]
    fn saved_heatmap_bounds_include_absolute_and_preinsertion_percentile_gates() {
        let mut settings = PercussionChannelSettings {
            min_reference: 1,
            sharpness_percentile: 100.0,
            energy_percentile: 100.0,
            ..fixed_floor_channel_settings()
        };
        let mut population = Population::default();
        population.insert(
            Observation {
                candidate_id: 9,
                time: 1.0,
                sharpness: 0.8,
                energy: 1e-4,
                ..Observation::default()
            },
            &settings,
        );
        let saved = population.snapshot(1.0, &settings);
        assert_eq!(saved.sharpness_cutoff, Some(0.8));
        assert_eq!(saved.energy_cutoff, Some(1e-4));
        assert!((saved.thresholds.sharpness_effective_cutoff - 0.79).abs() < 1e-6);
        assert!((saved.thresholds.energy_effective_cutoff - 0.000097).abs() < 1e-10);
        assert!(!saved.thresholds.warmup);
        assert_eq!(
            saved.thresholds.feature_seconds,
            settings.feature_ms * 0.001
        );
        settings.sharpness_percentile = 0.0;
        settings.energy_percentile = 0.0;
        let disabled = population.snapshot(1.0, &settings);
        assert_eq!(disabled.sharpness_cutoff, None);
        assert_eq!(disabled.energy_cutoff, None);
        assert_eq!(
            disabled.thresholds.sharpness_effective_cutoff,
            settings.min_sharpness
        );
        let energy_floor =
            (settings.feature_ms * 0.001 * 10.0_f32.powf(settings.min_attack_db / 10.0))
                .max(settings.admission_energy);
        assert_eq!(disabled.thresholds.energy_effective_cutoff, energy_floor);
        settings.min_reference = 2;
        settings.sharpness_percentile = 100.0;
        settings.energy_percentile = 100.0;
        let warmup = population.snapshot(1.0, &settings);
        assert!(warmup.thresholds.warmup);
        assert_eq!(warmup.sharpness_cutoff, None);
        assert_eq!(warmup.energy_cutoff, None);
        assert_eq!(saved.sharpness_cutoff, Some(0.8));
    }

    fn cloud_observation(
        bin: usize,
        time: f64,
        settings: &PercussionChannelSettings,
    ) -> Observation {
        Observation {
            time,
            sharpness: (time as f32 * 0.1).fract(),
            energy: energy_bin_edge(bin, settings.feature_ms * 0.001) * 1.1,
            ..Observation::default()
        }
    }

    #[test]
    fn cloud_ignores_sparse_tails_and_uses_top_percent_of_log_height() {
        let mut settings = PercussionChannelSettings {
            cloud_enabled: true,
            cloud_min_count: 3,
            cloud_top_percent: 25.0,
            ..fixed_floor_channel_settings()
        };
        let mut population = Population::default();
        assert!(population.snapshot(0.0, &settings).cloud_boundary.is_none());
        let fallback = population.snapshot(0.0, &settings).thresholds;
        assert!(!fallback.cloud_active);
        assert_eq!(
            fallback.trigger_energy_floor,
            settings.feature_ms * 0.001 * 10.0_f32.powf(settings.min_attack_db / 10.0)
        );
        for (index, bin) in [2, 9, 9, 9, 17, 17, 17, 22, 22].into_iter().enumerate() {
            population.insert(cloud_observation(bin, index as f64, &settings), &settings);
        }
        let saved = population.snapshot(9.0, &settings);
        let cloud = saved.cloud_boundary.unwrap();
        assert_eq!(
            cloud.energy_min,
            energy_bin_edge(9, settings.feature_ms * 0.001)
        );
        assert_eq!(
            cloud.energy_max,
            energy_bin_edge(18, settings.feature_ms * 0.001)
        );
        let floor_db =
            10.0 * (saved.thresholds.trigger_energy_floor / (settings.feature_ms * 0.001)).log10();
        assert!((floor_db - -27.5).abs() < 1e-5);
        assert!(saved.thresholds.cloud_active);
        let mut last_floor = f32::INFINITY;
        for percent in 0..=100 {
            settings.cloud_top_percent = percent as f32;
            let floor = population
                .snapshot(9.0, &settings)
                .thresholds
                .trigger_energy_floor;
            if percent == 0 {
                assert_eq!(floor, cloud.energy_max);
            } else if percent == 100 {
                assert_eq!(floor, cloud.energy_min);
            }
            assert!(
                floor <= last_floor,
                "larger top areas must be more permissive"
            );
            last_floor = floor;
        }
        assert!((last_floor - cloud.energy_min).abs() < 1e-12);
        population.insert(cloud_observation(22, 9.0, &settings), &settings);
        let expanded = population.snapshot(9.0, &settings).cloud_boundary.unwrap();
        assert_eq!(
            expanded.energy_max,
            energy_bin_edge(23, settings.feature_ms * 0.001)
        );
        assert_eq!(
            saved.cloud_boundary,
            Some(cloud),
            "historical bounds stay frozen"
        );
        settings.cloud_enabled = false;
        let disabled = population.snapshot(9.0, &settings);
        assert_eq!(
            disabled.cloud_boundary,
            Some(expanded),
            "tracking continues with fixed floor"
        );
        assert!(!disabled.thresholds.cloud_active);
        assert_eq!(
            disabled.thresholds.trigger_energy_floor,
            fallback.trigger_energy_floor
        );
    }

    #[test]
    fn cloud_contracts_on_time_and_capacity_eviction_and_handles_one_bucket() {
        let settings = PercussionChannelSettings {
            cloud_enabled: true,
            cloud_min_count: 3,
            memory_seconds: 1.0,
            ..fixed_floor_channel_settings()
        };
        let mut population = Population::default();
        for (index, bin) in [17, 17, 17, 9, 9, 9].into_iter().enumerate() {
            population.insert(
                Observation {
                    sharpness: (bin as f32 + 0.5) / HISTOGRAM_BINS as f32,
                    ..cloud_observation(bin, index as f64 * 0.1, &settings)
                },
                &settings,
            );
        }
        let full = population.snapshot(0.5, &settings).cloud_boundary.unwrap();
        let full_sharpness = population
            .snapshot(0.5, &settings)
            .sharpness_cloud_boundary
            .unwrap();
        assert_eq!(full_sharpness.sharpness_min, 9.0 / 24.0);
        assert_eq!(full_sharpness.sharpness_max, 18.0 / 24.0);
        population.expire(1.15, &settings);
        let shrunk = population.snapshot(1.15, &settings).cloud_boundary.unwrap();
        let shrunk_sharpness = population
            .snapshot(1.15, &settings)
            .sharpness_cloud_boundary
            .unwrap();
        assert_eq!(shrunk_sharpness.sharpness_min, full_sharpness.sharpness_min);
        assert_eq!(shrunk_sharpness.sharpness_max, 10.0 / 24.0);
        assert_eq!(shrunk.energy_min, full.energy_min);
        assert_eq!(
            shrunk.energy_max,
            energy_bin_edge(10, settings.feature_ms * 0.001)
        );
        assert!(shrunk.energy_max < full.energy_max);
        assert!(
            shrunk.energy_max > shrunk.energy_min,
            "one bucket retains its full height"
        );
        population.expire(2.0, &settings);
        assert!(population.snapshot(2.0, &settings).cloud_boundary.is_none());
        assert!(population
            .snapshot(2.0, &settings)
            .sharpness_cloud_boundary
            .is_none());
        for index in 0..REFERENCE_CAPACITY {
            let bin = if index < 3 { 17 } else { 9 };
            population.insert(
                Observation {
                    sharpness: (bin as f32 + 0.5) / HISTOGRAM_BINS as f32,
                    ..cloud_observation(bin, index as f64 * 0.001, &settings)
                },
                &settings,
            );
        }
        assert_eq!(
            population.snapshot(9.0, &settings).cloud_boundary,
            Some(full)
        );
        assert_eq!(
            population.snapshot(9.0, &settings).sharpness_cloud_boundary,
            Some(full_sharpness)
        );
        population.insert(
            Observation {
                sharpness: 9.5 / 24.0,
                ..cloud_observation(9, 9.0, &settings)
            },
            &settings,
        );
        assert_eq!(
            population.snapshot(9.0, &settings).cloud_boundary,
            Some(shrunk)
        );
        assert_eq!(
            population.snapshot(9.0, &settings).sharpness_cloud_boundary,
            Some(shrunk_sharpness)
        );
    }

    #[test]
    fn cloud_floor_uses_preinsertion_reference_and_has_no_upper_energy_limit() {
        let mut settings = permissive_filter_settings().channels[0];
        settings.filters.min_sharpness = false;
        settings.cloud_enabled = true;
        settings.cloud_min_count = 3;
        settings.cloud_top_percent = 25.0;
        // The fixed floor rejects the test candidate before a cloud exists.
        settings.min_attack_db = -10.0;
        let candidate = feature(0.002, 0.1, 0.001);
        let mut channel = Channel::default();
        for _ in 0..2 {
            let rejected = channel.finish(candidate, 0, 1.0, &settings);
            assert!(rejected.admitted && !rejected.eligible);
            assert!(!rejected.reference.thresholds.cloud_active);
        }
        let third = channel.finish(candidate, 0, 1.1, &settings);
        assert_eq!(third.reason, DecisionReason::AttackLevel);
        assert!(
            third.reference.cloud_boundary.is_none(),
            "candidate cannot supply its own Nth support"
        );
        assert!(
            channel
                .population
                .snapshot(1.1, &settings)
                .thresholds
                .cloud_active
        );
        let loud = channel.finish(feature(0.002, 0.2, 0.001), 0, 1.2, &settings);
        assert!(loud.energy > loud.reference.cloud_boundary.unwrap().energy_max);
        assert!(
            loud.eligible,
            "above-cloud energy has no upper rejection boundary"
        );
        let quiet = channel.finish(feature(0.002, 0.02, 0.001), 0, 1.3, &settings);
        assert!(quiet.admitted && !quiet.eligible);
        assert_eq!(quiet.reason, DecisionReason::AttackLevel);
        settings.filters.attack_level = false;
        assert_eq!(
            channel
                .finish(feature(0.002, 0.02, 0.001), 0, 1.4, &settings)
                .reason,
            DecisionReason::AttackLevel,
            "the fixed floor switch does not disable the cloud floor"
        );
        settings.cloud_enabled = false;
        assert!(
            channel
                .finish(feature(0.002, 0.02, 0.001), 0, 1.4, &settings)
                .eligible
        );
    }

    #[test]
    fn changing_cloud_controls_preserves_observations_epoch_and_saved_decisions() {
        let mut detector = SharpOnsets::default();
        let mut settings = permissive_filter_settings();
        feed_envelope(&mut detector, &settings, 0.0, 0.2, |at| {
            if (0.05..0.07).contains(&at) {
                0.2
            } else {
                0.0
            }
        });
        let saved = detector.decisions().clone();
        let epoch = detector.channels[0].epoch;
        settings.channels[0].cloud_enabled = true;
        settings.channels[0].cloud_sharpness_enabled = true;
        settings.channels[0].cloud_min_count = 1;
        settings.channels[0].cloud_top_percent = 75.0;
        settings.channels[0].cloud_sharpness_top_percent = 75.0;
        feed_envelope(&mut detector, &settings, 0.2, 0.05, |_| 0.0);
        let live = detector.reference(0);
        assert_eq!(live.count, 1);
        assert!(live.thresholds.cloud_active);
        assert!(live.thresholds.sharpness_cloud_active);
        assert_eq!(detector.channels[0].epoch, epoch);
        assert_eq!(detector.decisions(), &saved);
        settings.channels[0].cloud_sharpness_top_percent = 10.0;
        settings.channels[0].cloud_enabled = false;
        settings.channels[0].cloud_min_count = 2;
        feed_envelope(&mut detector, &settings, 0.25, 0.05, |_| 0.0);
        assert_eq!(detector.reference(0).count, 1);
        assert!(!detector.reference(0).thresholds.cloud_active);
        assert!(detector.reference(0).thresholds.cloud_enabled);
        assert!(detector.reference(0).thresholds.cloud_sharpness_enabled);
        assert_eq!(detector.channels[0].epoch, epoch);
        assert_eq!(detector.decisions(), &saved);
    }

    #[test]
    fn sharpness_cloud_ignores_sparse_tails_and_uses_linear_width() {
        let mut settings = permissive_filter_settings().channels[0];
        settings.cloud_sharpness_enabled = true;
        settings.cloud_min_count = 3;
        let mut population = Population::default();
        for (index, bin) in [2, 9, 9, 9, 17, 17, 17, 22, 22].into_iter().enumerate() {
            population.insert(
                Observation {
                    sharpness: (bin as f32 + 0.5) / HISTOGRAM_BINS as f32,
                    ..cloud_observation(index + 1, index as f64, &settings)
                },
                &settings,
            );
        }
        let saved = population.snapshot(9.0, &settings);
        assert!(
            saved.cloud_boundary.is_none(),
            "sharpness support pools different energy rows"
        );
        let cloud = saved.sharpness_cloud_boundary.unwrap();
        assert_eq!(cloud.sharpness_min, 9.0 / 24.0);
        assert_eq!(cloud.sharpness_max, 18.0 / 24.0);
        assert_eq!(saved.thresholds.trigger_sharpness_floor, 15.75 / 24.0);
        assert!(saved.thresholds.sharpness_cloud_active);
        assert!(!saved.thresholds.cloud_active);
        assert_eq!(
            saved.thresholds.trigger_energy_floor,
            settings.feature_ms * 0.001 * 10.0_f32.powf(settings.min_attack_db / 10.0)
        );
        let mut previous = f32::INFINITY;
        for percent in 0..=100 {
            settings.cloud_sharpness_top_percent = percent as f32;
            let floor = population
                .snapshot(9.0, &settings)
                .thresholds
                .trigger_sharpness_floor;
            assert!(floor <= previous);
            if percent == 0 {
                assert_eq!(floor, cloud.sharpness_max);
            }
            if percent == 100 {
                assert_eq!(floor, cloud.sharpness_min);
            }
            previous = floor;
        }
        population.insert(
            Observation {
                sharpness: 22.5 / 24.0,
                ..cloud_observation(10, 9.0, &settings)
            },
            &settings,
        );
        assert_eq!(
            population
                .snapshot(9.0, &settings)
                .sharpness_cloud_boundary
                .unwrap()
                .sharpness_max,
            23.0 / 24.0
        );
        assert_eq!(saved.sharpness_cloud_boundary, Some(cloud));
        settings.cloud_sharpness_enabled = false;
        let disabled = population.snapshot(9.0, &settings);
        assert!(disabled.sharpness_cloud_boundary.is_some());
        assert!(!disabled.thresholds.sharpness_cloud_active);
        assert_eq!(
            disabled.thresholds.trigger_sharpness_floor,
            settings.min_sharpness
        );
    }

    fn channel_with_cloud(settings: &PercussionChannelSettings) -> Channel {
        let mut channel = Channel::default();
        for (index, (energy_bin, sharpness_bin)) in
            [(12, 4), (18, 19)].into_iter().cycle().take(6).enumerate()
        {
            channel.population.insert(
                Observation {
                    sharpness: (sharpness_bin as f32 + 0.5) / HISTOGRAM_BINS as f32,
                    ..cloud_observation(energy_bin, 0.5 + index as f64 * 0.01, settings)
                },
                settings,
            );
        }
        channel
    }

    #[test]
    fn cloud_feature_floors_accept_either_branch_and_reject_only_when_both_fail() {
        let mut settings = permissive_filter_settings().channels[0];
        settings.cloud_enabled = true;
        settings.cloud_sharpness_enabled = true;
        settings.min_sharpness = 0.7;
        settings.min_attack_db = -20.0;
        for (rise, amplitude, energy_pass, sharpness_pass) in [
            (0.002, 0.01, false, true),
            (0.024, 0.3, true, false),
            (0.002, 0.3, true, true),
            (0.024, 0.01, false, false),
        ] {
            let decision = channel_with_cloud(&settings).finish(
                feature(rise, amplitude, 0.001),
                0,
                1.0,
                &settings,
            );
            let thresholds = decision.reference.thresholds;
            assert!(thresholds.cloud_active && thresholds.sharpness_cloud_active);
            assert_eq!(
                decision.energy >= thresholds.trigger_energy_floor,
                energy_pass
            );
            assert_eq!(
                decision.sharpness >= thresholds.trigger_sharpness_floor,
                sharpness_pass
            );
            assert_eq!(decision.eligible, energy_pass || sharpness_pass);
            if !decision.eligible {
                assert_eq!(decision.reason, DecisionReason::CloudRegion);
            }
            let static_settings = PercussionChannelSettings {
                cloud_enabled: false,
                cloud_sharpness_enabled: false,
                ..settings
            };
            let fixed = channel_with_cloud(&static_settings).finish(
                feature(rise, amplitude, 0.001),
                0,
                1.0,
                &static_settings,
            );
            assert_eq!(
                fixed.eligible,
                energy_pass && sharpness_pass,
                "fixed mode keeps AND"
            );
            // Both branches retain their fixed fallback when history is absent.
            let fallback =
                Channel::default().finish(feature(rise, amplitude, 0.001), 0, 1.0, &settings);
            assert!(
                !fallback.reference.thresholds.cloud_active
                    && !fallback.reference.thresholds.sharpness_cloud_active
            );
            assert_eq!(fallback.eligible, energy_pass || sharpness_pass);
        }
        settings.filters.attack_level = false;
        settings.filters.min_sharpness = false;
        for (energy_enabled, sharpness_enabled) in [(true, false), (false, true), (false, false)] {
            settings.cloud_enabled = energy_enabled;
            settings.cloud_sharpness_enabled = sharpness_enabled;
            for (rise, amplitude, energy_pass, sharpness_pass) in [
                (0.002, 0.01, false, true),
                (0.024, 0.3, true, false),
                (0.024, 0.01, false, false),
            ] {
                let decision = channel_with_cloud(&settings).finish(
                    feature(rise, amplitude, 0.001),
                    0,
                    1.0,
                    &settings,
                );
                assert_eq!(
                    decision.eligible,
                    (!energy_enabled && !sharpness_enabled)
                        || (energy_enabled && energy_pass)
                        || (sharpness_enabled && sharpness_pass),
                    "disabled branches cannot make OR pass"
                );
            }
        }
    }

    #[test]
    fn sharpness_cloud_uses_preinsertion_bounds_and_does_not_bypass_independent_filters() {
        let mut settings = permissive_filter_settings().channels[0];
        settings.cloud_sharpness_enabled = true;
        settings.filters.attack_level = false;
        settings.filters.min_sharpness = false;
        settings.min_sharpness = 0.9;
        let mut channel = Channel::default();
        for index in 0..3 {
            let decision = channel.finish(
                feature(0.01, 0.1, 0.001),
                0,
                1.0 + index as f64 * 0.01,
                &settings,
            );
            assert!(decision.admitted && !decision.eligible);
            assert_eq!(decision.reason, DecisionReason::PhysicalSharpness);
            assert!(decision.reference.sharpness_cloud_boundary.is_none());
        }
        let accepted = channel.finish(feature(0.002, 0.01, 0.001), 0, 1.1, &settings);
        assert!(accepted.eligible);
        assert!(
            accepted.sharpness
                > accepted
                    .reference
                    .sharpness_cloud_boundary
                    .unwrap()
                    .sharpness_max
        );
        settings.filters.mix_rms = true;
        settings.output_rms_db = 0.0;
        let rejected = channel.finish(feature(0.002, 0.01, 0.001), 0, 1.2, &settings);
        assert_eq!(rejected.reason, DecisionReason::MixRms);
        settings.filters.mix_rms = false;
        settings.filters.energy_percentile = true;
        settings.energy_percentile = 100.0;
        let rejected = channel.finish(feature(0.002, 0.01, 0.001), 0, 1.3, &settings);
        assert_eq!(rejected.reason, DecisionReason::EnergyPercentile);
    }

    #[test]
    fn sharpness_cloud_toggle_and_percentage_work_without_energy_or_fixed_floor_switches() {
        let mut settings = permissive_filter_settings();
        settings.channels[0].filters.attack_level = false;
        settings.channels[0].filters.min_sharpness = false;
        settings.channels[0].min_attack_db = -10.0;
        settings.channels[0].min_sharpness = 0.9;
        PercussionSetting::CloudSharpnessEnabled.set(&mut settings, 0, 1.0);
        for (percent, expected) in [(0.0, false), (100.0, true)] {
            PercussionSetting::CloudSharpnessTopPercent.set(&mut settings, 0, percent);
            let channel_settings = &settings.channels[0];
            let decision = channel_with_cloud(channel_settings).finish(
                feature(0.01, 0.1, 0.001),
                0,
                1.0,
                channel_settings,
            );
            let thresholds = decision.reference.thresholds;
            assert!(thresholds.cloud_enabled && thresholds.cloud_sharpness_enabled);
            assert!(!thresholds.cloud_energy_enabled);
            assert!(!thresholds.cloud_active && thresholds.sharpness_cloud_active);
            assert!(!thresholds.filters.attack_level && !thresholds.filters.min_sharpness);
            assert!(decision.energy < thresholds.trigger_energy_floor);
            assert_eq!(
                decision.eligible, expected,
                "sharpness area {percent}% must take effect directly"
            );
            if !expected {
                assert_eq!(decision.reason, DecisionReason::PhysicalSharpness);
            }
        }
        PercussionSetting::CloudSharpnessEnabled.set(&mut settings, 0, 0.0);
        settings.channels[0].filters.min_sharpness = true;
        let restored_fixed = channel_with_cloud(&settings.channels[0]).finish(
            feature(0.01, 0.1, 0.001),
            0,
            1.0,
            &settings.channels[0],
        );
        assert!(!restored_fixed.reference.thresholds.cloud_enabled);
        assert_eq!(restored_fixed.reason, DecisionReason::PhysicalSharpness);
    }

    #[test]
    fn output_rejected_candidates_appear_as_recent_learned_pairs_after_decision() {
        let settings = PercussionChannelSettings {
            output_rms_db: 0.0,
            min_reference: 1,
            ..fixed_floor_channel_settings()
        };
        let mut channel = Channel::default();
        let mut candidate = feature(0.002, 0.1, 0.001);
        candidate.id = 42;
        let decision = channel.finish(candidate, 0, 1.0, &settings);
        assert!(decision.admitted && !decision.eligible);
        assert_eq!(decision.reason, DecisionReason::MixRms);
        assert_eq!(decision.reference.count, 0);
        assert_eq!(
            decision.reference.recent_count, 0,
            "current candidate cannot enter its own reference"
        );
        let live = channel.population.snapshot(1.0, &settings);
        assert_eq!(live.count, 1);
        assert_eq!(live.recent_count, 1);
        assert_eq!(live.recent_points[0].candidate_id, 42);
        assert!(!live.recent_points[0].eligible);
        channel.reset_epoch();
        assert_eq!(channel.population.snapshot(1.0, &settings).recent_count, 0);
    }

    #[test]
    fn changing_recent_marker_count_preserves_populations_epochs_and_decisions() {
        let mut detector = SharpOnsets::default();
        let mut settings = fixed_floor_test_settings();
        feed_envelope(&mut detector, &settings, 0.0, 0.2, |at| {
            if (0.05..0.07).contains(&at) {
                0.2
            } else {
                0.0
            }
        });
        let saved = detector.decisions().clone();
        let epoch = detector.channels[0].epoch;
        settings.recent_markers = MAX_RECENT_MARKERS;
        feed_envelope(&mut detector, &settings, 0.2, 0.05, |_| 0.0);
        assert_eq!(detector.reference(0).count, 1);
        assert_eq!(detector.reference(0).recent_count, 1);
        assert_eq!(detector.channels[0].epoch, epoch);
        assert_eq!(detector.decisions(), &saved);
    }

    fn permissive_filter_settings() -> PercussionSettings {
        let channel = PercussionChannelSettings {
            admission_energy: 1e-10,
            admission_growth: 0.0001,
            admission_prominence: 0.01,
            min_sharpness: 0.1,
            min_output_prominence: 0.0,
            min_attack_db: -65.0,
            output_rms_db: -60.0,
            sharpness_percentile: 0.0,
            energy_percentile: 0.0,
            output_interval_ms: 5.0,
            min_reference: 1,
            ..fixed_floor_channel_settings()
        };
        PercussionSettings {
            channels: [channel; 3],
            ..fixed_floor_test_settings()
        }
    }

    fn contrast_candidate() -> Candidate {
        let settings = fixed_floor_channel_settings();
        let mut candidate = Candidate::new(1, 0.0, 0.08_f32.powi(2), 0.08, 0.08_f32.powi(2));
        for index in 0..24 {
            let amplitude = 0.08 + 0.02 * (((index + 1) as f32 * 0.001) / 0.002).min(1.0);
            candidate.push(amplitude, amplitude * amplitude, 0.25, 0.001, &settings);
        }
        candidate
    }

    #[test]
    fn each_floor_can_be_disabled_without_erasing_its_value_or_blocking_learning() {
        for (scalar, value, reason, candidate) in [
            (
                PercussionSetting::AdmissionEnergy,
                1e-4,
                DecisionReason::AdmissionEnergy,
                feature(0.002, 0.02, 0.001),
            ),
            (
                PercussionSetting::AdmissionGrowth,
                0.1,
                DecisionReason::NegligibleGrowth,
                feature(0.002, 0.02, 0.001),
            ),
            (
                PercussionSetting::AdmissionProminence,
                0.8,
                DecisionReason::Prominence,
                contrast_candidate(),
            ),
            (
                PercussionSetting::MinSharpness,
                0.9,
                DecisionReason::PhysicalSharpness,
                feature(0.024, 0.1, 0.001),
            ),
            (
                PercussionSetting::MinOutputProminence,
                0.8,
                DecisionReason::OutputProminence,
                contrast_candidate(),
            ),
            (
                PercussionSetting::MinAttackDb,
                -10.0,
                DecisionReason::AttackLevel,
                feature(0.002, 0.1, 0.001),
            ),
            (
                PercussionSetting::OutputRmsDb,
                0.0,
                DecisionReason::MixRms,
                feature(0.002, 0.1, 0.001),
            ),
        ] {
            let mut settings = permissive_filter_settings();
            scalar.set(&mut settings, 0, value);
            settings.validate().unwrap();
            let mut enabled_channel = Channel::default();
            let rejected = enabled_channel.finish(candidate, 0, 1.0, &settings.channels[0]);
            assert!(!rejected.eligible, "{scalar:?}");
            assert_eq!(rejected.reason, reason, "{scalar:?}");
            let retained_value = scalar.get(&settings, 0);
            scalar.filter_toggle().unwrap().set(&mut settings, 0, 0.0);
            assert_eq!(scalar.get(&settings, 0), retained_value);
            let mut disabled_channel = Channel::default();
            let accepted = disabled_channel.finish(candidate, 0, 1.0, &settings.channels[0]);
            assert!(
                accepted.admitted && accepted.eligible,
                "{scalar:?}: {:?}",
                accepted.reason
            );
            assert_eq!(disabled_channel.population.paired.len(), 1);
            assert_eq!(accepted.reference.count, 0);
        }
    }

    #[test]
    fn percentile_spacing_and_overlap_filters_can_each_be_disabled() {
        for (scalar, reason, candidate, prior) in [
            (
                PercussionSetting::SharpnessPercentile,
                DecisionReason::SharpnessPercentile,
                feature(0.024, 0.1, 0.001),
                Observation {
                    time: 0.5,
                    sharpness: 0.9,
                    energy: 1e-10,
                    ..Observation::default()
                },
            ),
            (
                PercussionSetting::EnergyPercentile,
                DecisionReason::EnergyPercentile,
                feature(0.002, 0.02, 0.001),
                Observation {
                    time: 0.5,
                    sharpness: 0.1,
                    energy: 1e-3,
                    ..Observation::default()
                },
            ),
        ] {
            let mut settings = permissive_filter_settings();
            scalar.set(&mut settings, 0, 100.0);
            let mut channel = Channel::default();
            channel.population.insert(prior, &settings.channels[0]);
            let rejected = channel.finish(candidate, 0, 1.0, &settings.channels[0]);
            assert_eq!(rejected.reason, reason);
            assert!(rejected.admitted && !rejected.eligible);
            scalar.filter_toggle().unwrap().set(&mut settings, 0, 0.0);
            let accepted = channel.finish(candidate, 0, 1.1, &settings.channels[0]);
            assert!(accepted.admitted && accepted.eligible);
            if scalar == PercussionSetting::SharpnessPercentile {
                assert_eq!(accepted.reference.sharpness_cutoff, None);
            } else {
                assert_eq!(accepted.reference.energy_cutoff, None);
            }
        }
        let mut settings = permissive_filter_settings();
        settings.channels[0].output_interval_ms = 300.0;
        let mut channel = Channel {
            last_output: 1.0,
            ..Channel::default()
        };
        let rejected = channel.finish(feature(0.002, 0.1, 0.001), 0, 1.001, &settings.channels[0]);
        assert_eq!(rejected.reason, DecisionReason::Refractory);
        settings.channels[0].filters.output_interval = false;
        assert!(
            channel
                .finish(feature(0.002, 0.1, 0.001), 0, 1.002, &settings.channels[0])
                .eligible
        );
        let mut candidate = feature(0.002, 0.1, 0.001);
        candidate.overlap = true;
        let rejected = channel.finish(candidate, 0, 2.0, &settings.channels[0]);
        assert_eq!(rejected.reason, DecisionReason::Overlap);
        settings.channels[0].filters.reject_overlap = false;
        let accepted = channel.finish(candidate, 0, 2.1, &settings.channels[0]);
        assert!(accepted.admitted && accepted.eligible && accepted.overlapping);
        assert!(
            channel
                .population
                .recent_admissions
                .back()
                .unwrap()
                .overlapping
        );
    }

    #[test]
    fn disabled_axis_filters_remove_heatmap_bounds_and_keep_numerical_protection() {
        let mut settings = permissive_filter_settings();
        let mut detector = SharpOnsets::default();
        feed_envelope(&mut detector, &settings, 0.0, 0.2, |at| {
            if (0.05..0.07).contains(&at) {
                0.2
            } else {
                0.0
            }
        });
        let old_decisions = detector.decisions().clone();
        let old_epoch = detector.channels[0].epoch;
        for scalar in PercussionSetting::CHANNEL {
            if scalar.is_filter_toggle() {
                scalar.set(&mut settings, 0, 0.0);
            }
        }
        feed_envelope(&mut detector, &settings, 0.2, 0.05, |_| 0.0);
        let live = detector.reference(0);
        assert_eq!(live.count, 1);
        assert_eq!(live.thresholds.sharpness_effective_bound, None);
        assert_eq!(live.thresholds.energy_effective_bound, None);
        assert_eq!(detector.channels[0].epoch, old_epoch);
        assert_eq!(detector.decisions(), &old_decisions);
        assert!(old_decisions[0].reference.thresholds.filters.input_energy);
        let mut invalid = feature(0.002, 0.1, 0.001);
        invalid.invalid = true;
        assert_eq!(
            detector.channels[0]
                .finish(invalid, 0, 1.0, &settings.channels[0])
                .reason,
            DecisionReason::Numerical
        );
        let mut silent = feature(0.002, 0.1, 0.001);
        silent.growth = 0.0;
        assert_eq!(
            detector.channels[0]
                .finish(silent, 0, 1.1, &settings.channels[0])
                .reason,
            DecisionReason::NegligibleGrowth
        );
    }

    fn feed_envelope(
        detector: &mut SharpOnsets,
        settings: &PercussionSettings,
        start: f64,
        seconds: f32,
        amplitude: impl Fn(f32) -> f32,
    ) {
        let dt = 0.001;
        for index in 0..(seconds / dt).round() as usize {
            let at = (index + 1) as f32 * dt;
            let value = amplitude(at);
            let frame = PercussionFrame {
                power: [value * value; 3],
                stereo_power: [[value * value; 2]; 3],
                available: [true; 3],
                rms: 0.3,
                dt,
                at: start + f64::from(at),
                ..PercussionFrame::default()
            };
            detector.observe_percussion(&frame, frame.at, settings);
        }
    }

    #[test]
    fn repeated_equal_attacks_create_one_observation_each_without_tail_duplicates() {
        let mut detector = SharpOnsets::default();
        let settings = fixed_floor_test_settings();
        feed_envelope(&mut detector, &settings, 0.0, 4.0, |at| {
            if at < 0.2 {
                return 0.0;
            }
            let within = (at - 0.2).rem_euclid(0.25);
            0.2 * (within / 0.002).min(1.0) * (-within / 0.025).exp()
        });
        for channel in 0..3 {
            let records: Vec<_> = detector
                .decisions()
                .iter()
                .filter(|d| d.channel == channel && d.admitted)
                .collect();
            assert_eq!(records.len(), 16, "channel {channel}: {records:?}");
            assert!(records.iter().all(|d| d.eligible));
        }
    }

    #[test]
    fn slow_attack_does_not_restart_when_it_later_crosses_admission_or_rms() {
        let mut detector = SharpOnsets::default();
        let settings = fixed_floor_test_settings();
        feed_envelope(&mut detector, &settings, 0.0, 0.6, |at| {
            ((at - 0.1) / 0.12).clamp(0.0, 1.0) * 0.2
        });
        for channel in 0..3 {
            let records: Vec<_> = detector
                .decisions()
                .iter()
                .filter(|d| d.channel == channel)
                .collect();
            assert_eq!(
                records.len(),
                1,
                "slow attack must retain its early boundary"
            );
            assert!(records[0].onset_time < 0.11);
            assert!(!records[0].eligible);
            assert!(records[0].rise_censored);
        }
    }

    #[test]
    fn smooth_only_reference_never_bypasses_physical_output_sanity() {
        let mut channel = Channel::default();
        let settings = PercussionChannelSettings {
            min_reference: 1,
            ..fixed_floor_channel_settings()
        };
        for index in 0..40 {
            let decision = channel.finish(feature(0.120, 0.5, 0.001), 0, index as f64, &settings);
            assert!(decision.admitted);
            assert!(!decision.eligible);
            assert_eq!(decision.reason, DecisionReason::PhysicalSharpness);
        }
    }

    #[test]
    fn tiny_crackle_over_loud_bed_does_not_borrow_background_energy() {
        let mut channel = Channel::default();
        let settings = fixed_floor_channel_settings();
        let mut candidate = Candidate::new(1, 0.0, 0.01, 0.7, 0.49);
        for _ in 0..24 {
            candidate.push(0.70001, 0.490014, 0.49, 0.001, &settings);
        }
        let decision = channel.finish(candidate, 0, 1.0, &settings);
        assert!(!decision.admitted && !decision.eligible);
        assert_eq!(decision.reason, DecisionReason::NegligibleGrowth);
    }

    #[test]
    fn close_attacks_remain_explicit_records_with_overlap_rejections() {
        let mut detector = SharpOnsets::default();
        let mut settings = fixed_floor_test_settings();
        for channel in &mut settings.channels {
            channel.rearm_ms = 3.0;
        }
        feed_envelope(&mut detector, &settings, 0.0, 0.2, |at| {
            let pulse = |start: f32| {
                let age = at - start;
                if age >= 0.0 {
                    0.2 * (age / 0.001).min(1.0) * (-age / 0.002).exp()
                } else {
                    0.0
                }
            };
            pulse(0.05) + pulse(0.065)
        });
        for channel in 0..3 {
            let records: Vec<_> = detector
                .decisions()
                .iter()
                .filter(|d| d.channel == channel)
                .collect();
            assert_eq!(records.len(), 2);
            assert!(records.iter().all(|d| d.reason == DecisionReason::Overlap));
            assert!(records[1].onset_time > records[0].onset_time);
        }
    }

    #[test]
    fn epoch_resets_are_local_and_cutoff_changes_preserve_observations() {
        let mut detector = SharpOnsets::default();
        let mut settings = fixed_floor_test_settings();
        feed_envelope(&mut detector, &settings, 0.0, 0.2, |at| {
            if (0.05..0.07).contains(&at) {
                0.2
            } else {
                0.0
            }
        });
        assert_eq!(detector.reference(0).count, 1);
        settings.channels[0].sharpness_percentile = 100.0;
        feed_envelope(&mut detector, &settings, 0.2, 0.05, |_| 0.0);
        assert_eq!(detector.reference(0).count, 1);
        settings.channels[0].feature_ms = 25.0;
        feed_envelope(&mut detector, &settings, 0.25, 0.05, |_| 0.0);
        assert_eq!(detector.reference(0).count, 0);
        assert_eq!(detector.reference(1).count, 1);
        assert_eq!(detector.reference(2).count, 1);
    }

    #[test]
    fn audio_time_expiry_runs_on_unavailable_channels_and_after_day_long_clocks() {
        let mut detector = SharpOnsets::default();
        let settings = fixed_floor_test_settings();
        feed_envelope(&mut detector, &settings, 86_400.0, 0.2, |at| {
            if (0.05..0.07).contains(&at) {
                0.2
            } else {
                0.0
            }
        });
        assert_eq!(detector.reference(0).count, 1);
        let saved = detector.decisions()[0].clone();
        assert!(saved.completion_time > saved.onset_time + 0.023);
        let frame = PercussionFrame {
            at: 86_430.0,
            dt: 0.001,
            ..PercussionFrame::default()
        };
        detector.observe_percussion(&frame, frame.at, &settings);
        for channel in 0..3 {
            assert_eq!(detector.reference(channel).count, 0);
        }
        assert_eq!(saved.reference.count, 0);
    }

    fn production(seconds: f32, mut sample: impl FnMut(f32) -> f32) -> SharpOnsets {
        use super::super::percussion_spectrum::PercussionAnalyzer;
        let settings = fixed_floor_test_settings();
        let mut analyzer = PercussionAnalyzer::new(48_000);
        analyzer.set_settings(&settings);
        let mut detector = SharpOnsets::default();
        for index in 0..(seconds * 48_000.0).round() as usize {
            let value = sample(index as f32 / 48_000.0);
            if let Some(frame) = analyzer.push_stereo([value, -value]) {
                detector.observe_percussion(&frame, frame.at, &settings);
            }
        }
        detector
    }

    #[test]
    fn production_sustained_tones_sweeps_vibrato_and_tremolo_do_not_chatter() {
        let tau = std::f32::consts::TAU;
        for frequency in [60.0, 80.0, 120.0, 180.0] {
            for phase in [0.0, 0.3, 1.7] {
                let detector = production(0.7, |at| 0.4 * (tau * frequency * at + phase).sin());
                assert!(
                    detector
                        .channels
                        .iter()
                        .all(|channel| channel.eligible == 0),
                    "held {frequency} phase {phase}: {:?}",
                    detector.decisions()
                );
            }
        }
        let mut phase = 0.0;
        let vibrato = production(2.0, |at| {
            phase += tau * (120.0 + 8.0 * (tau * 6.0 * at).sin()) / 48_000.0;
            0.4 * phase.sin()
        });
        assert_eq!(
            vibrato.channels[0].eligible,
            0,
            "vibrato: {:?}",
            vibrato.decisions()
        );
        let mut phase = 0.0;
        let sweep = production(2.0, |at| {
            phase += tau * (60.0 + at * 60.0) / 48_000.0;
            0.4 * phase.sin()
        });
        assert_eq!(
            sweep.channels[0].eligible,
            0,
            "sweep: {:?}",
            sweep.decisions()
        );
        let tremolo = production(2.0, |at| {
            0.4 * (0.8 + 0.2 * (tau * 4.0 * at).sin()) * (tau * 120.0 * at).sin()
        });
        assert_eq!(
            tremolo.channels[0].eligible,
            0,
            "tremolo: {:?}",
            tremolo.decisions()
        );
    }

    #[test]
    fn production_steady_noise_and_quiet_junk_alone_or_over_a_loud_bed_do_not_trigger() {
        for gain in [1.0, 0.3, 0.003] {
            let mut state = 0x7139_4a2bu32;
            let noise = production(3.0, |_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                gain * (state as f32 / u32::MAX as f32 * 2.0 - 1.0)
            });
            assert!(
                noise.channels.iter().all(|channel| channel.eligible == 0),
                "steady noise gain {gain}, counts {:?}",
                noise.channels.each_ref().map(|channel| channel.eligible)
            );
        }
        for bed in [0.0, 0.5] {
            let mut state = 0x7139_4a2bu32;
            let junk = production(2.0, |at| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let within = at.rem_euclid(0.125);
                let crackle =
                    0.0005 * (-within / 0.001).exp() * (state as f32 / u32::MAX as f32 * 2.0 - 1.0);
                bed * (std::f32::consts::TAU * 440.0 * at).sin() + crackle
            });
            assert!(
                junk.channels.iter().all(|channel| channel.eligible == 0),
                "quiet crackle over bed {bed}"
            );
        }
        let click = production(0.7, |at| {
            if (at - 0.25).abs() < 0.00001 {
                1.0
            } else {
                0.0
            }
        });
        assert!(click.channels.iter().all(|channel| channel.eligible == 0));
    }

    #[test]
    fn mixed_horizons_publish_a_monotonic_cursor_without_losing_candidate_identity() {
        let mut detector = SharpOnsets::default();
        let mut settings = fixed_floor_test_settings();
        settings.channels[0].feature_ms = 40.0;
        feed_envelope(&mut detector, &settings, 0.0, 0.2, |at| {
            if (0.05..0.06).contains(&at) {
                0.2
            } else {
                0.0
            }
        });
        let records: Vec<_> = detector.decisions().iter().collect();
        assert_eq!(records.len(), 3);
        assert!(records.windows(2).all(|pair| pair[0].id < pair[1].id));
        assert!(records
            .windows(2)
            .all(|pair| pair[0].completion_time <= pair[1].completion_time));
        assert_eq!(records.last().unwrap().channel, 0);
        assert!(records.last().unwrap().candidate_id < records[0].candidate_id);
    }

    #[test]
    fn a_dying_neighbor_can_mask_regional_energy_while_per_bin_excess_overcounts_pitch_motion() {
        let horizon = 0.024;
        let frozen = [0.04_f32, 0.0];
        let mixture = [0.01_f32, 0.02];
        let regional =
            (mixture.iter().sum::<f32>() - frozen.iter().sum::<f32>()).max(0.0) * horizon;
        let per_bin = mixture
            .iter()
            .zip(frozen)
            .map(|(current, old)| (current - old).max(0.0))
            .sum::<f32>()
            * horizon;
        assert_eq!(regional, 0.0);
        assert!((per_bin - 0.02 * horizon).abs() < 1e-9);
        let moved_pitch = [0.0_f32, 0.04];
        let motion_excess = moved_pitch
            .iter()
            .zip(frozen)
            .map(|(current, old)| (current - old).max(0.0))
            .sum::<f32>()
            * horizon;
        assert!((motion_excess - 0.04 * horizon).abs() < 1e-9);
        assert_eq!(moved_pitch.iter().sum::<f32>(), frozen.iter().sum::<f32>());
    }

    #[test]
    fn production_repeated_kicks_stay_audible_over_an_unrelated_midrange_bed() {
        let pulse = |at: f32| {
            if at < 0.25 {
                return 0.0;
            }
            let age = (at - 0.25).rem_euclid(0.5);
            0.4 * (age / 0.001).min(1.0)
                * (-age / 0.06).exp()
                * (std::f32::consts::TAU * 120.0 * age).sin()
        };
        let alone = production(2.2, pulse);
        let mixed = production(2.2, |at| {
            pulse(at) + 0.5 * (std::f32::consts::TAU * 440.0 * at).sin()
        });
        assert_eq!(alone.channels[0].eligible, 4);
        assert_eq!(mixed.channels[0].eligible, 4);
        let decisions: Vec<_> = mixed
            .decisions()
            .iter()
            .filter(|decision| decision.channel == 0 && decision.admitted)
            .collect();
        assert_eq!(decisions.len(), 4);
        assert!(decisions.iter().all(|decision| decision.eligible));
    }

    #[test]
    fn pending_capacity_is_a_visible_rejection_and_preamp_reset_preserves_identity() {
        let mut channel = Channel {
            initialized: true,
            ..Channel::default()
        };
        channel
            .pending
            .fill(Some(Candidate::new(1, 1.0, 0.0, 0.0, 0.0)));
        let mut candidate_id = 10;
        let mut publication_id = 10;
        let mut decisions = VecDeque::with_capacity(DECISION_CAPACITY);
        channel.observe(
            [0.04; 2],
            true,
            0.3,
            0,
            0.02,
            0.001,
            1.01,
            &fixed_floor_channel_settings(),
            0,
            &mut candidate_id,
            &mut publication_id,
            &mut decisions,
        );
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].reason, DecisionReason::Capacity);
        assert_eq!(decisions[0].feature_seconds, 0.0);
        assert!(!decisions[0].admitted && !decisions[0].eligible);
        assert_eq!(decisions[0].sharpness_rank, None);
        let mut detector = SharpOnsets::default();
        let settings = fixed_floor_test_settings();
        feed_envelope(&mut detector, &settings, 0.0, 0.2, |at| {
            if (0.05..0.07).contains(&at) {
                0.2
            } else {
                0.0
            }
        });
        let saved = detector.decisions().clone();
        let next_id = detector.next_id;
        let cursor = detector.next_decision_id;
        detector.reset_features();
        assert_eq!(detector.decisions(), &saved);
        assert_eq!(detector.next_id, next_id);
        assert_eq!(detector.next_decision_id, cursor);
        assert!(detector
            .channels
            .iter()
            .all(|channel| channel.population.paired.is_empty()));
    }
}
impl Default for Population {
    fn default() -> Self {
        Self {
            paired: VecDeque::with_capacity(REFERENCE_CAPACITY),
            recent_admissions: VecDeque::with_capacity(MAX_RECENT_MARKERS),
            sharpness: Vec::with_capacity(REFERENCE_CAPACITY),
            energy: Vec::with_capacity(REFERENCE_CAPACITY),
            sharpness_histogram: [0; HISTOGRAM_BINS],
            energy_histogram: [0; HISTOGRAM_BINS],
            joint_histogram: [[0; HEATMAP_BINS]; HEATMAP_BINS],
            last_capacity_removal: f64::NEG_INFINITY,
        }
    }
}
fn sharpness_bin(value: f32) -> usize {
    ((value.clamp(0.0, 1.0) * HISTOGRAM_BINS as f32) as usize).min(HISTOGRAM_BINS - 1)
}
fn energy_bin(value: f32, horizon: f32) -> usize {
    let db = 10.0 * (value / horizon).max(1e-12).log10();
    (((db + 80.0) / 80.0 * HISTOGRAM_BINS as f32).clamp(0.0, (HISTOGRAM_BINS - 1) as f32)) as usize
}
fn energy_bin_edge(index: usize, horizon: f32) -> f32 {
    let db = -80.0 + 80.0 * index as f32 / HISTOGRAM_BINS as f32;
    horizon * 10.0_f32.powf(db / 10.0)
}
fn supported_bin_range(
    histogram: &[u32; HISTOGRAM_BINS],
    minimum: usize,
) -> Option<(usize, usize)> {
    let supported = |count: &u32| *count as usize >= minimum;
    Some((
        histogram.iter().position(supported)?,
        histogram.iter().rposition(supported)?,
    ))
}
fn remove_value(values: &mut Vec<f32>, value: f32) {
    let index = values
        .binary_search_by(|v| v.total_cmp(&value))
        .expect("paired and sorted references agree");
    values.remove(index);
}
fn quantile(values: &[f32], percentile: f32) -> Option<f32> {
    if values.is_empty() || percentile <= 0.0 {
        return None;
    }
    let index = ((percentile / 100.0 * values.len() as f32).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    Some(values[index])
}
fn rank(values: &[f32], value: f32) -> RankInterval {
    RankInterval {
        lower: values.partition_point(|v| *v < value) as f32 / values.len() as f32,
        upper: values.partition_point(|v| *v <= value) as f32 / values.len() as f32,
    }
}
impl Population {
    fn cloud_boundary(&self, settings: &PercussionChannelSettings) -> Option<CloudBoundary> {
        // Marginal energy counts pool all sharpness columns. Sparse tails stay
        // visible and learn normally, but cannot stretch either cloud edge.
        let (first, last) = supported_bin_range(&self.energy_histogram, settings.cloud_min_count)?;
        let horizon = settings.feature_ms * 0.001;
        Some(CloudBoundary {
            energy_min: energy_bin_edge(first, horizon),
            energy_max: energy_bin_edge(last + 1, horizon),
        })
    }
    fn sharpness_cloud_boundary(
        &self,
        settings: &PercussionChannelSettings,
    ) -> Option<SharpnessCloudBoundary> {
        let (first, last) =
            supported_bin_range(&self.sharpness_histogram, settings.cloud_min_count)?;
        Some(SharpnessCloudBoundary {
            sharpness_min: first as f32 / HISTOGRAM_BINS as f32,
            sharpness_max: (last + 1) as f32 / HISTOGRAM_BINS as f32,
        })
    }
    fn remove_oldest(&mut self, horizon: f32) {
        if let Some(old) = self.paired.pop_front() {
            remove_value(&mut self.sharpness, old.sharpness);
            remove_value(&mut self.energy, old.energy);
            self.sharpness_histogram[sharpness_bin(old.sharpness)] -= 1;
            self.energy_histogram[energy_bin(old.energy, horizon)] -= 1;
            self.joint_histogram[energy_bin(old.energy, horizon)][sharpness_bin(old.sharpness)] -=
                1;
        }
    }
    fn expire(&mut self, now: f64, settings: &PercussionChannelSettings) {
        while self
            .paired
            .front()
            .is_some_and(|old| now - old.time > f64::from(settings.memory_seconds))
        {
            self.remove_oldest(settings.feature_ms * 0.001);
        }
    }
    fn insert(&mut self, observation: Observation, settings: &PercussionChannelSettings) {
        if self.paired.len() == REFERENCE_CAPACITY {
            self.last_capacity_removal = self.paired.front().unwrap().time;
            self.remove_oldest(settings.feature_ms * 0.001);
        }
        let index = self
            .sharpness
            .partition_point(|v| *v <= observation.sharpness);
        self.sharpness.insert(index, observation.sharpness);
        let index = self.energy.partition_point(|v| *v <= observation.energy);
        self.energy.insert(index, observation.energy);
        self.sharpness_histogram[sharpness_bin(observation.sharpness)] += 1;
        self.energy_histogram[energy_bin(observation.energy, settings.feature_ms * 0.001)] += 1;
        self.joint_histogram[energy_bin(observation.energy, settings.feature_ms * 0.001)]
            [sharpness_bin(observation.sharpness)] += 1;
        self.paired.push_back(observation);
        if self.recent_admissions.len() == MAX_RECENT_MARKERS {
            self.recent_admissions.pop_front();
        }
        self.recent_admissions.push_back(observation);
    }
    fn snapshot(
        &self,
        now: f64,
        settings: &PercussionChannelSettings,
    ) -> PercussionReferenceSnapshot {
        let ready = self.paired.len() >= settings.min_reference;
        let sharpness_cutoff = (ready && settings.filters.sharpness_percentile)
            .then(|| quantile(&self.sharpness, settings.sharpness_percentile))
            .flatten();
        let energy_cutoff = (ready && settings.filters.energy_percentile)
            .then(|| quantile(&self.energy, settings.energy_percentile))
            .flatten();
        let feature_seconds = settings.feature_ms * 0.001;
        let min_attack_energy = feature_seconds * 10.0_f32.powf(settings.min_attack_db / 10.0);
        let cloud_boundary = self.cloud_boundary(settings);
        let sharpness_cloud_boundary = self.sharpness_cloud_boundary(settings);
        let cloud_mode = settings.cloud_enabled || settings.cloud_sharpness_enabled;
        let cloud_active = settings.cloud_enabled && cloud_boundary.is_some();
        let sharpness_cloud_active =
            settings.cloud_sharpness_enabled && sharpness_cloud_boundary.is_some();
        let trigger_energy_floor = if let Some(cloud) = cloud_boundary.filter(|_| cloud_active) {
            // Interpolate in the map's logarithmic energy coordinate: top 25%
            // accepts the upper quarter of its height, plus everything louder.
            if settings.cloud_top_percent == 0.0 {
                cloud.energy_max
            } else if settings.cloud_top_percent == 100.0 {
                cloud.energy_min
            } else {
                let fraction = 1.0 - settings.cloud_top_percent / 100.0;
                cloud.energy_min * (cloud.energy_max / cloud.energy_min).powf(fraction)
            }
        } else {
            min_attack_energy
        };
        let trigger_sharpness_floor =
            if let Some(cloud) = sharpness_cloud_boundary.filter(|_| sharpness_cloud_active) {
                if settings.cloud_sharpness_top_percent == 0.0 {
                    cloud.sharpness_max
                } else if settings.cloud_sharpness_top_percent == 100.0 {
                    cloud.sharpness_min
                } else {
                    cloud.sharpness_max
                        - (cloud.sharpness_max - cloud.sharpness_min)
                            * settings.cloud_sharpness_top_percent
                            / 100.0
                }
            } else {
                settings.min_sharpness
            };
        // Cloud floors form a union, so they cannot be represented by one
        // pair of lower bounds. Keep only independent AND constraints here.
        let sharpness_effective_bound = [
            (settings.filters.min_sharpness && !cloud_mode).then_some(settings.min_sharpness),
            sharpness_cutoff.map(|q| q - SHARPNESS_TOLERANCE),
        ]
        .into_iter()
        .flatten()
        .reduce(f32::max);
        let energy_effective_bound = [
            (settings.filters.attack_level && !cloud_mode).then_some(trigger_energy_floor),
            settings
                .filters
                .input_energy
                .then_some(settings.admission_energy),
            energy_cutoff.map(|q| q - (q * ENERGY_RELATIVE_TOLERANCE).max(1e-12)),
        ]
        .into_iter()
        .flatten()
        .reduce(f32::max);
        let thresholds = PercussionThresholdSnapshot {
            filters: settings.filters,
            min_sharpness: settings.min_sharpness,
            min_attack_db: settings.min_attack_db,
            trigger_energy_floor,
            trigger_sharpness_floor,
            cloud_enabled: cloud_mode,
            cloud_energy_enabled: settings.cloud_enabled,
            cloud_sharpness_enabled: settings.cloud_sharpness_enabled,
            cloud_active,
            sharpness_cloud_active,
            cloud_top_percent: settings.cloud_top_percent,
            cloud_sharpness_top_percent: settings.cloud_sharpness_top_percent,
            cloud_min_count: settings.cloud_min_count,
            admission_energy: settings.admission_energy,
            admission_growth: settings.admission_growth,
            admission_prominence: settings.admission_prominence,
            min_output_prominence: settings.min_output_prominence,
            output_rms_db: settings.output_rms_db,
            output_interval_ms: settings.output_interval_ms,
            feature_seconds,
            sharpness_effective_cutoff: sharpness_effective_bound.unwrap_or(0.0),
            energy_effective_cutoff: energy_effective_bound.unwrap_or(0.0),
            sharpness_effective_bound,
            energy_effective_bound,
            sharpness_tolerance: SHARPNESS_TOLERANCE,
            energy_relative_tolerance: ENERGY_RELATIVE_TOLERANCE,
            energy_absolute_tolerance: 1e-12,
            warmup: !ready,
        };
        let mut recent_points = [PercussionReferencePoint::default(); MAX_RECENT_MARKERS];
        let recent_count = self.recent_admissions.len();
        for (index, observation) in self.recent_admissions.iter().enumerate() {
            recent_points[index] = PercussionReferencePoint {
                candidate_id: observation.candidate_id,
                time: observation.time,
                sharpness: observation.sharpness,
                energy: observation.energy,
                eligible: observation.eligible,
                provisional: observation.provisional,
                overlapping: observation.overlapping,
                in_reference: self.paired.front().is_some_and(|first| {
                    (observation.time, observation.candidate_id) >= (first.time, first.candidate_id)
                }),
            };
        }
        PercussionReferenceSnapshot {
            count: self.paired.len(),
            oldest_time: self.paired.front().map(|v| v.time),
            newest_time: self.paired.back().map(|v| v.time),
            effective_seconds: self
                .paired
                .front()
                .map_or(0.0, |v| (now - v.time).max(0.0) as f32),
            capacity_truncated: !self.paired.is_empty()
                && now - self.last_capacity_removal <= f64::from(settings.memory_seconds),
            sharpness_histogram: self.sharpness_histogram,
            energy_histogram: self.energy_histogram,
            joint_histogram: self.joint_histogram,
            cloud_boundary,
            sharpness_cloud_boundary,
            recent_points,
            recent_count,
            thresholds,
            sharpness_cutoff,
            energy_cutoff,
            ..PercussionReferenceSnapshot::default()
        }
    }
}

#[derive(Clone, Copy, Default)]
struct GrowthSegment {
    start: f32,
    end: f32,
    growth: f32,
}
#[derive(Clone, Copy)]
struct Candidate {
    id: u64,
    start: f64,
    background: f32,
    stereo_background: [f32; 2],
    previous_amplitudes: [f32; 2],
    previous_powers: [f32; 2],
    elapsed: f32,
    absolute_energy: f32,
    energy: f32,
    mix_energy: f32,
    growth: f32,
    peak: f32,
    peak_excess: f32,
    segments: [GrowthSegment; MAX_SEGMENTS],
    segments_len: usize,
    envelope: [EnvelopePoint; ENVELOPE_POINTS],
    envelope_len: usize,
    overlap: bool,
    invalid: bool,
    last_growth: f32,
}
impl Candidate {
    #[cfg(test)]
    fn new(id: u64, start: f64, background: f32, amplitude: f32, power: f32) -> Self {
        Self::new_stereo(id, start, [background; 2], [amplitude; 2], [power; 2])
    }

    fn new_stereo(
        id: u64,
        start: f64,
        background: [f32; 2],
        amplitudes: [f32; 2],
        powers: [f32; 2],
    ) -> Self {
        let amplitude = stereo_rms(amplitudes);
        let mut value = Self {
            id,
            start,
            background: stereo_power_mean(background),
            stereo_background: background,
            previous_amplitudes: amplitudes,
            previous_powers: powers,
            elapsed: 0.0,
            absolute_energy: 0.0,
            energy: 0.0,
            mix_energy: 0.0,
            growth: 0.0,
            peak: amplitude,
            peak_excess: stereo_rms(std::array::from_fn(|side| {
                (amplitudes[side] - background[side].sqrt()).max(0.0)
            })),
            segments: [GrowthSegment::default(); MAX_SEGMENTS],
            segments_len: 0,
            envelope: [EnvelopePoint::default(); ENVELOPE_POINTS],
            envelope_len: 1,
            overlap: false,
            invalid: false,
            last_growth: 0.0,
        };
        value.envelope[0] = EnvelopePoint {
            seconds: 0.0,
            amplitude,
            power: stereo_power_mean(powers),
            left_amplitude: amplitudes[0],
            right_amplitude: amplitudes[1],
            cumulative_growth: 0.0,
        };
        value
    }
    #[cfg(test)]
    fn push(
        &mut self,
        amplitude: f32,
        power: f32,
        mix_power: f32,
        dt: f32,
        settings: &PercussionChannelSettings,
    ) {
        self.push_stereo([amplitude; 2], [power; 2], mix_power, dt, settings);
    }

    fn push_stereo(
        &mut self,
        amplitudes: [f32; 2],
        powers: [f32; 2],
        mix_power: f32,
        dt: f32,
        settings: &PercussionChannelSettings,
    ) {
        let horizon = settings.feature_ms * 0.001;
        let used = dt.min((horizon - self.elapsed).max(0.0));
        if used <= 0.0 {
            return;
        }
        let fraction = used / dt;
        let amplitudes = std::array::from_fn(|side| {
            self.previous_amplitudes[side]
                + (amplitudes[side] - self.previous_amplitudes[side]) * fraction
        });
        let powers = std::array::from_fn(|side| {
            self.previous_powers[side] + (powers[side] - self.previous_powers[side]) * fraction
        });
        let amplitude = stereo_rms(amplitudes);
        let power = stereo_power_mean(powers);
        self.absolute_energy += (stereo_power_mean(self.previous_powers) + power) * 0.5 * used;
        // Integrate positive excess separately before averaging energy. A
        // decaying left side must not consume new right-side attack energy.
        let mut excess = 0.0;
        for (side, power) in powers.into_iter().enumerate() {
            let lo = self.previous_powers[side] - self.stereo_background[side];
            let hi = power - self.stereo_background[side];
            excess += if lo >= 0.0 && hi >= 0.0 {
                (lo + hi) * 0.5
            } else if lo <= 0.0 && hi <= 0.0 {
                0.0
            } else {
                let positive = lo.max(hi);
                positive * positive / (2.0 * (hi - lo).abs().max(1e-20))
            };
        }
        self.energy += excess * 0.5 * used;
        self.mix_energy += mix_power * used;
        let growth = stereo_rms(std::array::from_fn(|side| {
            (amplitudes[side] - self.previous_amplitudes[side]).max(0.0)
        }));
        self.growth += growth;
        self.last_growth = growth;
        if self.segments_len < MAX_SEGMENTS {
            self.segments[self.segments_len] = GrowthSegment {
                start: self.elapsed,
                end: self.elapsed + used,
                growth,
            };
            self.segments_len += 1;
        } else {
            self.invalid = true;
        }
        self.elapsed += used;
        self.peak = self.peak.max(amplitude);
        self.peak_excess = self.peak_excess.max(stereo_rms(std::array::from_fn(|side| {
            (amplitudes[side] - self.stereo_background[side].sqrt()).max(0.0)
        })));
        // Uniform display grid never feeds feature extraction.
        let index = ((self.elapsed / horizon * (ENVELOPE_POINTS - 1) as f32).round() as usize)
            .min(ENVELOPE_POINTS - 1);
        self.envelope[index] = EnvelopePoint {
            seconds: self.elapsed,
            amplitude,
            power,
            left_amplitude: amplitudes[0],
            right_amplitude: amplitudes[1],
            cumulative_growth: self.growth,
        };
        self.envelope_len = self.envelope_len.max(index + 1);
        self.previous_amplitudes = amplitudes;
        self.previous_powers = powers;
    }
    fn concentration(&self, delta: f32) -> f32 {
        if self.growth < 1e-9 {
            return 0.0;
        }
        let mut maximum = 0.0_f32;
        // Positive growth is uniform inside each measured hop. Window maxima
        // occur at segment edges or edges shifted by the concentration window.
        for segment in &self.segments[..self.segments_len] {
            for end in [segment.end, (segment.start + delta).min(self.elapsed)] {
                let start = (end - delta).max(0.0);
                let mut total = 0.0;
                for part in &self.segments[..self.segments_len] {
                    let overlap = (part.end.min(end) - part.start.max(start)).max(0.0);
                    total += part.growth * overlap / (part.end - part.start).max(1e-12);
                }
                maximum = maximum.max(total);
            }
        }
        (maximum / self.growth).clamp(0.0, 1.0)
    }
    fn compact_envelope(&mut self) {
        let mut length = 0;
        for index in 0..self.envelope_len {
            if index == 0 || self.envelope[index].seconds > 0.0 {
                self.envelope[length] = self.envelope[index];
                length += 1;
            }
        }
        self.envelope_len = length;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventState {
    Idle,
    Collecting,
    Finalizing,
    Rearming,
}
struct Channel {
    population: Population,
    // Allocate bounded feature storage once. Stereo diagnostic traces make
    // inline candidates large enough to overflow small UI/audio thread stacks.
    pending: Box<[Option<Candidate>; MAX_PENDING]>,
    state: EventState,
    amplitude: f32,
    background: f32,
    stereo_amplitudes: [f32; 2],
    stereo_powers: [f32; 2],
    stereo_background: [f32; 2],
    noise_growth: f32,
    quiet_seconds: f32,
    last_start: f64,
    last_output: f64,
    initialized: bool,
    available: bool,
    flash: f32,
    strength: f32,
    epoch: u64,
    candidates: u64,
    admitted: u64,
    eligible: u64,
    settings: Option<PercussionChannelSettings>,
    revision: Option<u64>,
    latest_sharpness: f32,
    latest_energy: f32,
    latest_mix_db: f32,
    latest_reference: Option<PercussionReferenceSnapshot>,
    now: f64,
    support: f32,
}
impl Default for Channel {
    fn default() -> Self {
        Self {
            population: Population::default(),
            pending: Box::new([None; MAX_PENDING]),
            state: EventState::Idle,
            amplitude: 0.0,
            background: 0.0,
            stereo_amplitudes: [0.0; 2],
            stereo_powers: [0.0; 2],
            stereo_background: [0.0; 2],
            noise_growth: 0.0,
            quiet_seconds: 0.0,
            last_start: f64::NEG_INFINITY,
            last_output: f64::NEG_INFINITY,
            initialized: false,
            available: false,
            flash: 0.0,
            strength: 0.0,
            epoch: 0,
            candidates: 0,
            admitted: 0,
            eligible: 0,
            settings: None,
            revision: None,
            latest_sharpness: 0.0,
            latest_energy: 0.0,
            latest_mix_db: -120.0,
            latest_reference: None,
            now: 0.0,
            support: 0.0,
        }
    }
}
fn feature_changed(a: PercussionChannelSettings, b: PercussionChannelSettings) -> bool {
    a.low_hz != b.low_hz
        || a.high_hz != b.high_hz
        || a.taper_hz != b.taper_hz
        || a.feature_ms != b.feature_ms
        || a.concentration_ms != b.concentration_ms
        || a.envelope_ms != b.envelope_ms
        || a.background_ms != b.background_ms
        || a.merge_ms != b.merge_ms
        || a.rearm_ms != b.rearm_ms
}
impl Channel {
    fn reset_epoch(&mut self) {
        self.population.paired.clear();
        self.population.recent_admissions.clear();
        self.population.sharpness.clear();
        self.population.energy.clear();
        self.population.sharpness_histogram.fill(0);
        self.population.energy_histogram.fill(0);
        self.population.joint_histogram.fill([0; HEATMAP_BINS]);
        self.population.last_capacity_removal = f64::NEG_INFINITY;
        self.pending.fill(None);
        self.state = EventState::Idle;
        self.initialized = false;
        self.quiet_seconds = 0.0;
        self.noise_growth = 0.0;
        self.flash = 0.0;
        self.latest_reference = None;
        self.latest_sharpness = 0.0;
        self.latest_energy = 0.0;
        self.latest_mix_db = -120.0;
        self.last_start = f64::NEG_INFINITY;
        self.last_output = f64::NEG_INFINITY;
        self.epoch = self.epoch.wrapping_add(1);
    }
    fn status(&self, settings: &PercussionChannelSettings) -> PercussionChannelStatus {
        PercussionChannelStatus {
            envelope: self.amplitude,
            background_power: self.background,
            available: self.available,
            collecting: self.pending.iter().any(Option::is_some),
            epoch: self.epoch,
            candidates: self.candidates,
            admitted: self.admitted,
            eligible: self.eligible,
            reference: self.population.snapshot(self.now, settings),
        }
    }
    fn finish(
        &mut self,
        mut candidate: Candidate,
        channel: usize,
        now: f64,
        settings: &PercussionChannelSettings,
    ) -> PercussionDecision {
        candidate.compact_envelope();
        self.population.expire(now, settings);
        let sharpness = candidate.concentration(settings.concentration_ms * 0.001);
        let new_growth = candidate
            .peak_excess
            .min(candidate.growth)
            .min(candidate.peak);
        let prominence = new_growth / candidate.peak.max(1e-12);
        let reference = self.population.snapshot(now, settings);
        let ready = reference.count >= settings.min_reference;
        let mut reason = DecisionReason::Eligible;
        let admitted = if candidate.invalid
            || !candidate.energy.is_finite()
            || !sharpness.is_finite()
        {
            reason = DecisionReason::Numerical;
            false
        } else if settings.filters.reject_overlap && candidate.overlap {
            reason = DecisionReason::Overlap;
            false
        } else if (settings.filters.input_growth && new_growth < settings.admission_growth)
            || candidate.growth < 1e-8
        {
            reason = DecisionReason::NegligibleGrowth;
            false
        } else if settings.filters.input_energy && candidate.energy < settings.admission_energy {
            reason = DecisionReason::AdmissionEnergy;
            false
        } else if settings.filters.input_prominence && prominence < settings.admission_prominence {
            reason = DecisionReason::Prominence;
            false
        } else {
            true
        };
        let sharpness_rank = ready.then(|| rank(&self.population.sharpness, sharpness));
        let energy_rank = ready.then(|| rank(&self.population.energy, candidate.energy));
        let mix_rms_db = 10.0
            * (candidate.mix_energy / candidate.elapsed.max(1e-9))
                .max(1e-12)
                .log10();
        let attack_db = 10.0
            * (candidate.energy / candidate.elapsed.max(1e-9))
                .max(1e-12)
                .log10();
        let cloud_mode = settings.cloud_enabled || settings.cloud_sharpness_enabled;
        let cloud_rejection = if cloud_mode {
            let energy_pass = settings.cloud_enabled
                && candidate.energy >= reference.thresholds.trigger_energy_floor;
            let sharpness_pass = settings.cloud_sharpness_enabled
                && sharpness >= reference.thresholds.trigger_sharpness_floor;
            match (
                settings.cloud_enabled,
                settings.cloud_sharpness_enabled,
                energy_pass || sharpness_pass,
            ) {
                (true, true, false) => Some(DecisionReason::CloudRegion),
                (true, false, false) => Some(DecisionReason::AttackLevel),
                (false, true, false) => Some(DecisionReason::PhysicalSharpness),
                _ => None,
            }
        } else {
            None
        };
        let mut tolerance_used = false;
        let eligible = if !admitted {
            false
        } else if let Some(rejection) = cloud_rejection {
            reason = rejection;
            false
        } else if !cloud_mode
            && settings.filters.min_sharpness
            && sharpness < settings.min_sharpness
        {
            reason = DecisionReason::PhysicalSharpness;
            false
        } else if settings.filters.output_prominence && prominence < settings.min_output_prominence
        {
            reason = DecisionReason::OutputProminence;
            false
        } else if !cloud_mode && settings.filters.attack_level && attack_db < settings.min_attack_db
        {
            reason = DecisionReason::AttackLevel;
            false
        } else if settings.filters.mix_rms && mix_rms_db < settings.output_rms_db {
            reason = DecisionReason::MixRms;
            false
        } else if reference
            .sharpness_cutoff
            .is_some_and(|q| sharpness < q - SHARPNESS_TOLERANCE)
        {
            reason = DecisionReason::SharpnessPercentile;
            false
        } else if reference
            .energy_cutoff
            .is_some_and(|q| candidate.energy < q - (q * ENERGY_RELATIVE_TOLERANCE).max(1e-12))
        {
            reason = DecisionReason::EnergyPercentile;
            false
        } else if settings.filters.output_interval
            && now - self.last_output < f64::from(settings.output_interval_ms) * 0.001
        {
            reason = DecisionReason::Refractory;
            false
        } else {
            tolerance_used = reference.sharpness_cutoff.is_some_and(|q| sharpness < q)
                || reference
                    .energy_cutoff
                    .is_some_and(|q| candidate.energy < q);
            reason = if ready {
                DecisionReason::Eligible
            } else {
                DecisionReason::Provisional
            };
            true
        };
        if admitted {
            self.population.insert(
                Observation {
                    candidate_id: candidate.id,
                    time: now,
                    sharpness,
                    energy: candidate.energy,
                    eligible,
                    provisional: !ready,
                    overlapping: candidate.overlap,
                },
                settings,
            );
            self.admitted += 1;
        }
        if eligible {
            self.last_output = now;
            self.flash = 1.0;
            self.strength =
                ((candidate.energy / candidate.elapsed.max(1e-9)).sqrt() * 4.0).clamp(0.15, 1.0);
            self.eligible += 1;
        }
        self.latest_sharpness = sharpness;
        self.latest_energy = candidate.energy;
        self.latest_reference = Some(reference);
        PercussionDecision {
            id: candidate.id,
            candidate_id: candidate.id,
            channel,
            epoch: self.epoch,
            onset_time: candidate.start,
            completion_time: now,
            emission_time: now,
            sharpness,
            energy: candidate.energy,
            absolute_energy: candidate.absolute_energy,
            growth: new_growth,
            prominence,
            background_power: candidate.background,
            background_stereo_power: candidate.stereo_background,
            peak_amplitude: candidate.peak,
            mix_rms_db,
            admitted,
            eligible,
            provisional: !ready,
            overlapping: candidate.overlap,
            rise_censored: candidate.last_growth > candidate.growth * 0.005,
            tolerance_used,
            reason,
            sharpness_rank,
            energy_rank,
            reference,
            envelope: candidate.envelope,
            envelope_len: candidate.envelope_len,
            feature_seconds: candidate.elapsed,
            support_seconds: self.support,
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn observe(
        &mut self,
        powers: [f32; 2],
        available: bool,
        rms: f32,
        revision: u64,
        support: f32,
        dt: f32,
        now: f64,
        settings: &PercussionChannelSettings,
        channel: usize,
        next_id: &mut u64,
        next_decision_id: &mut u64,
        decisions: &mut VecDeque<PercussionDecision>,
    ) -> Option<f32> {
        if self.revision.is_some_and(|old| old != revision)
            || self
                .settings
                .is_some_and(|old| feature_changed(old, *settings))
        {
            self.reset_epoch();
        }
        self.revision = Some(revision);
        self.settings = Some(*settings);
        self.now = now;
        self.available = available;
        self.support = support;
        self.population.expire(now, settings);
        self.flash *= (-8.0 * dt).exp();
        if !available {
            self.pending.fill(None);
            self.initialized = false;
            return None;
        }
        if powers
            .iter()
            .any(|power| !power.is_finite() || *power < 0.0)
            || !rms.is_finite()
            || !dt.is_finite()
            || dt <= 0.0
        {
            self.pending.fill(None);
            self.initialized = false;
            return None;
        }
        let power = stereo_power_mean(powers);
        let raw_amplitudes = powers.map(f32::sqrt);
        self.latest_mix_db = 20.0 * rms.max(1e-6).log10();
        if !self.initialized {
            self.stereo_amplitudes = raw_amplitudes;
            self.stereo_powers = powers;
            self.stereo_background = powers;
            self.amplitude = stereo_rms(raw_amplitudes);
            self.background = power;
            self.initialized = true;
            return None;
        }
        let previous = self.amplitude;
        let previous_amplitudes = self.stereo_amplitudes;
        let previous_powers = self.stereo_powers;
        let smoothing = 1.0 - (-dt / (settings.envelope_ms * 0.001)).exp();
        for (side, raw) in raw_amplitudes.into_iter().enumerate() {
            self.stereo_amplitudes[side] += smoothing * (raw - self.stereo_amplitudes[side]);
        }
        self.amplitude = stereo_rms(self.stereo_amplitudes);
        let movement =
            std::array::from_fn(|side| self.stereo_amplitudes[side] - previous_amplitudes[side]);
        let increase = stereo_rms(movement.map(|value| value.max(0.0)));
        // Numerical discovery floor lies well below histogram admission. Start
        // gradual notes before absolute energy or output RMS gates open.
        let discovery = 1e-6_f32.max(previous * 0.001).max(self.noise_growth * 1.5);
        // -80 dBFS regional amplitude is still well below admission/output
        // floors, but excludes finite-FFT leakage/numerical ripples which can
        // otherwise keep a selected silent region permanently collecting.
        let fresh = increase > discovery && self.amplitude >= 0.0001;
        // Noise adaptation must not declare a still-rising slow attack settled.
        if increase <= 1e-6_f32.max(self.amplitude * 0.001) {
            self.quiet_seconds += dt;
        } else {
            self.quiet_seconds = 0.0;
        }
        if self.state != EventState::Idle && self.quiet_seconds >= settings.rearm_ms * 0.001 {
            self.state = EventState::Idle;
        }
        let can_split = self.state != EventState::Idle
            && self.quiet_seconds == 0.0
            && now - self.last_start >= f64::from(settings.merge_ms) * 0.001
            && self.pending.iter().flatten().any(|pending| {
                pending.last_growth <= 1e-7
                    && increase > stereo_rms(pending.previous_amplitudes) * 0.02
            });
        if fresh && (self.state == EventState::Idle || can_split) {
            if let Some(slot) = self.pending.iter().position(Option::is_none) {
                let overlap = self.pending.iter().any(Option::is_some);
                if overlap {
                    for old in self.pending.iter_mut().flatten() {
                        old.overlap = true;
                    }
                }
                let mut candidate = Candidate::new_stereo(
                    *next_id,
                    now - f64::from(dt),
                    self.stereo_background,
                    previous_amplitudes,
                    previous_powers,
                );
                candidate.overlap = overlap;
                *next_id = next_id.wrapping_add(1);
                self.pending[slot] = Some(candidate);
                self.candidates += 1;
                self.state = EventState::Collecting;
                self.last_start = now;
            } else {
                // Queue exhaustion is a visible bounded rejection, not a lost
                // candidate or an invented fixed-horizon measurement.
                let rejected = Candidate::new_stereo(
                    *next_id,
                    now - f64::from(dt),
                    self.stereo_background,
                    previous_amplitudes,
                    previous_powers,
                );
                *next_id = next_id.wrapping_add(1);
                self.candidates += 1;
                let mut decision = self.finish(rejected, channel, now, settings);
                decision.reason = DecisionReason::Capacity;
                decision.sharpness_rank = None;
                decision.energy_rank = None;
                decision.rise_censored = true;
                decision.id = *next_decision_id;
                *next_decision_id = next_decision_id.wrapping_add(1);
                if decisions.len() == DECISION_CAPACITY {
                    decisions.pop_front();
                }
                decisions.push_back(decision);
                self.last_start = now;
                self.state = EventState::Collecting;
            }
        }
        self.stereo_powers = powers;
        let mut emitted = None;
        for index in 0..MAX_PENDING {
            if let Some(candidate) = &mut self.pending[index] {
                candidate.push_stereo(self.stereo_amplitudes, powers, rms * rms, dt, settings);
                if candidate.elapsed + 1e-7 >= settings.feature_ms * 0.001 {
                    self.state = EventState::Finalizing;
                    let candidate = self.pending[index].take().unwrap();
                    let mut decision = self.finish(candidate, channel, now, settings);
                    decision.id = *next_decision_id;
                    *next_decision_id = next_decision_id.wrapping_add(1);
                    if decision.eligible {
                        emitted = Some(self.strength);
                    }
                    if decisions.len() == DECISION_CAPACITY {
                        decisions.pop_front();
                    }
                    decisions.push_back(decision);
                    self.state = EventState::Rearming;
                }
            }
        }
        // Causal noise/background followers support extraction only.
        let alpha = 1.0 - (-dt / (settings.background_ms * 0.001)).exp();
        for (side, power) in powers.into_iter().enumerate() {
            self.stereo_background[side] += alpha * (power - self.stereo_background[side]);
        }
        self.background = stereo_power_mean(self.stereo_background);
        self.noise_growth += alpha * (stereo_rms(movement) - self.noise_growth);
        emitted
    }
}

pub(super) struct SharpOnsets {
    channels: [Channel; 3],
    decisions: VecDeque<PercussionDecision>,
    next_id: u64,
    next_decision_id: u64,
}
impl Default for SharpOnsets {
    fn default() -> Self {
        Self {
            channels: std::array::from_fn(|_| Channel::default()),
            decisions: VecDeque::with_capacity(DECISION_CAPACITY),
            next_id: 1,
            next_decision_id: 1,
        }
    }
}
impl SharpOnsets {
    /// A preamp epoch changes the feature scale while preserving publication
    /// cursors and immutable history on the continuing audio clock.
    pub fn reset_features(&mut self) {
        for channel in &mut self.channels {
            channel.reset_epoch();
        }
    }
    pub fn observe_percussion(
        &mut self,
        frame: &PercussionFrame,
        now: f64,
        settings: &PercussionSettings,
    ) -> DetectorEvents {
        let outputs = std::array::from_fn::<_, 3, _>(|channel| {
            self.channels[channel].observe(
                frame.stereo_power[channel],
                frame.available[channel],
                frame.rms,
                frame.revisions[channel],
                frame.window_seconds[channel],
                frame.dt,
                now,
                &settings.channels[channel],
                channel,
                &mut self.next_id,
                &mut self.next_decision_id,
                &mut self.decisions,
            )
        });
        DetectorEvents {
            kick: outputs[0],
            snare: outputs[1],
            hat: outputs[2],
            ..DetectorEvents::default()
        }
    }
    pub fn decisions(&self) -> &VecDeque<PercussionDecision> {
        &self.decisions
    }
    pub fn reference(&self, channel: usize) -> PercussionReferenceSnapshot {
        let current = &self.channels[channel];
        current
            .population
            .snapshot(current.now, &current.settings.unwrap_or_default())
    }
    pub fn channel_status(&self, channel: usize) -> PercussionChannelStatus {
        let current = &self.channels[channel];
        current.status(&current.settings.unwrap_or_default())
    }
    pub fn triggers(&self) -> [TriggerStatus; 3] {
        self.channels.each_ref().map(|channel| TriggerStatus {
            flash: channel.flash,
            power: channel.strength,
            count: 0,
            armed: false,
            choked: false,
        })
    }
    pub fn fluxes(&self) -> [OnsetFluxStatus; 3] {
        self.channels.each_ref().map(|channel| {
            let snapshot = channel.latest_reference.unwrap_or_default();
            OnsetFluxStatus {
                flux: channel.latest_sharpness,
                smoothed: channel.amplitude,
                sharpness: channel.latest_sharpness,
                adaptation_input: channel.latest_energy,
                baseline: channel.background.sqrt(),
                reference: snapshot.sharpness_cutoff,
                threshold: snapshot.sharpness_cutoff.unwrap_or(0.0),
                mix_gate: channel.latest_mix_db
                    >= channel.settings.unwrap_or_default().output_rms_db,
                ..OnsetFluxStatus::default()
            }
        })
    }
}
