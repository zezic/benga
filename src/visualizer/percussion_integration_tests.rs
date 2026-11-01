use super::*;

fn fixture(rate: usize) -> Vec<[f32; 2]> {
    let mut noise = 0x4592_3271_u32;
    (0..rate * 2)
        .map(|i| {
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            let time = i as f64 / rate as f64;
            let age = (time - 0.25).rem_euclid(0.5);
            let env = (-age / 0.04).exp() as f32;
            let value = 0.4 * env * (std::f64::consts::TAU * 120.0 * time).sin() as f32
                + 0.15 * env * (noise as f32 / u32::MAX as f32 * 2.0 - 1.0);
            [value, -value]
        })
        .collect()
}

#[test]
fn independent_percussion_preserves_every_arrangement_input_and_hop() {
    for rate in [8_000, 16_000, 44_100, 48_000, 96_000] {
        let mut visualizer = Visualizer::new();
        visualizer.set_samplerate(rate);
        let mut original = SpectrumAnalyzer::new(rate);
        original.set_relative_excess(visualizer.detectors.excess);
        let mut hops = 0;
        for input in fixture(rate) {
            let expected = original.push_stereo(input);
            visualizer.consume_buf(&[input], 1.0);
            if let Some(expected) = expected {
                hops += 1;
                let actual = visualizer.last_spectrum;
                assert_eq!(actual.rms.to_bits(), expected.rms.to_bits());
                for (actual, expected) in [
                    actual.bands,
                    actual.linear_bands,
                    actual.band_excess,
                ]
                .into_iter()
                .zip([
                    expected.bands,
                    expected.linear_bands,
                    expected.band_excess,
                ]) {
                    assert_eq!(
                        actual.map(f32::to_bits),
                        expected.map(f32::to_bits),
                        "rate {rate}, hop {hops}"
                    );
                }
            }
            assert_eq!(visualizer.analysis_revision(), hops);
        }
    }
}

#[test]
fn callbacks_do_not_change_decisions_features_or_emission_times() {
    let input = fixture(48_000);
    let run = |partition| {
        let mut visualizer = Visualizer::new();
        for callback in input.chunks(partition) {
            visualizer.consume_buf(callback, 0.5);
        }
        (
            visualizer.percussion_decisions().clone(),
            visualizer.trigger_counts(),
        )
    };
    let expected = run(48);
    assert!(!expected.0.is_empty());
    for partition in [1, 137, 256, 4096, 8192] {
        assert_eq!(run(partition), expected, "callback {partition}");
    }
}

#[test]
fn heatmap_marker_count_is_persisted_without_changing_learning_or_triggers() {
    let input = fixture(48_000);
    let mut baseline = Visualizer::new();
    let mut edited = Visualizer::new();
    let marker_count = Parameter::Percussion(Detector::Kick, PercussionSetting::RecentMarkers);
    for (index, callback) in input.chunks(137).enumerate() {
        if index == 10 {
            edited.set_parameter(marker_count, 4.0);
        }
        if index == 300 {
            edited.set_parameter(marker_count, 64.0);
        }
        baseline.consume_buf(callback, 0.5);
        edited.consume_buf(callback, 0.5);
    }
    assert_eq!(edited.parameter_value(marker_count), 64.0);
    assert_eq!(edited.trigger_counts(), baseline.trigger_counts());
    assert_eq!(
        edited.percussion_decisions(),
        baseline.percussion_decisions()
    );
    for channel in 0..3 {
        assert_eq!(
            edited.percussion_reference(channel),
            baseline.percussion_reference(channel)
        );
    }
    let settings = edited.detector_settings();
    let restored: DetectorSettings = ron::from_str(&ron::to_string(&settings).unwrap()).unwrap();
    assert_eq!(restored, settings);
}

#[test]
fn preamp_change_starts_a_new_population_without_a_fake_attack_or_clock_reset() {
    let mut visualizer = Visualizer::new();
    visualizer.consume_buf(&fixture(48_000), 1.0);
    let before = visualizer.percussion_audio_time();
    let saved = visualizer.percussion_decisions().clone();
    let revision_before = visualizer.analysis_revision();
    visualizer.consume_buf(&vec![[0.0; 2]; 4800], 0.5);
    assert_eq!(
        visualizer.percussion_decisions(),
        &saved,
        "preamp edits preserve immutable history and its identities"
    );
    assert_eq!(visualizer.percussion_audio_time(), before + 0.1);
    assert!(visualizer.analysis_revision() > revision_before);
    for channel in 0..3 {
        assert_eq!(visualizer.percussion_reference(channel).count, 0);
    }
    let last_id = saved.back().unwrap().id;
    let max_epoch = saved.iter().map(|decision| decision.epoch).max().unwrap();
    visualizer.consume_buf(&fixture(48_000), 0.5);
    let new = visualizer
        .percussion_decisions()
        .iter()
        .filter(|decision| decision.id > last_id)
        .collect::<Vec<_>>();
    assert!(!new.is_empty());
    assert!(new.iter().all(|decision| decision.epoch > max_epoch));
}

#[test]
fn histogram_revision_migration_validates_then_resets_only_percussion() {
    let saved = "(onset_revision:2,kick_low_hz:75.0,kick_high_hz:410.0,kick_quake_factor:17.0,arrangement:(gap_hold:2.0),bin_gains:(kick:(points:[(hz:100.0,gain:0.2)])))";
    let migrated: DetectorSettings = ron::from_str(saved).unwrap();
    assert_eq!(migrated.percussion, PercussionSettings::default());
    assert_eq!(migrated.bin_gains, PercussionBinGains::default());
    assert_eq!(migrated.kick_quake_factor, 17.0);
    assert_eq!(migrated.arrangement.gap_hold, 2.0);
    for invalid in [
        "(onset_revision:4)",
        "(onset_revision:2,kick_low_hz:NaN)",
        "(onset_revision:2,bin_gains:(kick:(points:[(hz:100.0,gain:NaN)])))",
        "(onset_revision:2,flux_attack:NaN)",
        "(onset_revision:2,drop_gap_window:NaN)",
        "(onset_revision:2,drop_window:NaN)",
        "(onset_revision:2,snare_min_power:NaN)",
    ] {
        assert!(
            ron::from_str::<DetectorSettings>(invalid).is_err(),
            "{invalid}"
        );
    }
    let mut custom = migrated;
    PercussionSetting::SharpnessPercentile.set(&mut custom.percussion, 0, 99.0);
    custom
        .bin_gains
        .set_bin(0, BinAddress::new(100.0, 25.0), 0.4);
    let restored: DetectorSettings = ron::from_str(&ron::to_string(&custom).unwrap()).unwrap();
    assert_eq!(restored, custom);
}
