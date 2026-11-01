//! Stereo regressions exercise the production stream rather than a mixed
//! waveform. Channel motion is measured before its positive changes combine.
use super::*;

fn fixed_floor_settings() -> DetectorSettings {
    let mut settings = DetectorSettings {
        percussion: percussion::fixed_floor_test_settings(),
        ..DetectorSettings::default()
    };
    for channel in 0..3 {
        settings.bin_gains.reset(channel);
    }
    settings
}

fn stream(
    rate: usize,
    seconds: f64,
    partition: usize,
    settings: DetectorSettings,
    mut signal: impl FnMut(f64) -> [f32; 2],
) -> Visualizer {
    let mut visualizer = Visualizer::new();
    visualizer.set_samplerate(rate);
    visualizer.set_detector_settings(settings);
    let mut frames = Vec::with_capacity(partition);
    for index in 0..(seconds * rate as f64).round() as usize {
        frames.push(signal(index as f64 / rate as f64));
        if frames.len() == partition {
            visualizer.consume_buf(&frames, 1.0);
            frames.clear();
        }
    }
    visualizer.consume_buf(&frames, 1.0);
    visualizer
}

fn carrier(time: f64, instrument: usize, side: usize, phase: f64) -> f64 {
    let frequencies: &[f64] = match (instrument, side) {
        (1, 0) => &[2812.5, 3375.0, 3937.5],
        (1, _) => &[3000.0, 3562.5, 4125.0],
        (_, 0) => &[7500.0, 9000.0, 11250.0],
        _ => &[8250.0, 10125.0, 12375.0],
    };
    frequencies
        .iter()
        .map(|frequency| (std::f64::consts::TAU * frequency * time + phase).sin())
        .sum::<f64>()
        / (frequencies.len() as f64).sqrt()
}

fn attack(age: f64, instrument: usize, side: usize, phase: f64) -> f64 {
    if age < 0.0 {
        return 0.0;
    }
    0.45 * (age / 0.001).min(1.0) * (-age / 0.025).exp() * carrier(age, instrument, side, phase)
}

#[test]
fn a_new_side_attack_is_not_canceled_by_the_other_sides_decay() {
    for instrument in [1, 2] {
        let result = stream(48_000, 0.30, 137, fixed_floor_settings(), |time| {
            let transition = ((time - 0.15) / 0.004).clamp(0.0, 1.0);
            let angle = transition * std::f64::consts::FRAC_PI_2;
            [
                (0.5 * angle.cos() * carrier(time, instrument, 0, 0.7)) as f32,
                (0.5 * angle.sin() * carrier(time, instrument, 1, 1.3)) as f32,
            ]
        });
        let records: Vec<_> = result
            .percussion_decisions()
            .iter()
            .filter(|decision| {
                decision.channel == instrument && (0.145..0.180).contains(&decision.onset_time)
            })
            .collect();
        assert!(records.iter().any(|decision| decision.admitted && decision.eligible
            && decision.energy > 1e-5 && decision.growth > 0.01),
            "instrument {instrument}: a substantial right-channel attack must remain visible while left-channel power falls; records={records:?}");
        assert_eq!(
            records.iter().filter(|decision| decision.admitted).count(),
            1,
            "one stereo handoff must add one paired observation"
        );
        eprintln!(
            "stereo handoff instrument={instrument}: {:?}",
            records
                .iter()
                .map(|decision| (
                    decision.onset_time,
                    decision.energy,
                    decision.growth,
                    decision.sharpness
                ))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn centered_panned_antiphase_and_wide_hf_hits_emit_once_within_50ms() {
    let mut delays = Vec::new();
    for rate in [44_100, 48_000, 96_000] {
        for instrument in [1, 2] {
            for phase in [0.0, 0.9] {
                for layout in 0..8 {
                    let result = stream(rate, 0.21, 137, fixed_floor_settings(), |time| {
                        let age = time - 0.1;
                        let left = attack(age, instrument, 0, phase);
                        let right = attack(age, instrument, 1, phase + 0.7);
                        match layout {
                            0 => [left as f32; 2],
                            1 => [left as f32, -left as f32],
                            2 => [(std::f64::consts::SQRT_2 * left) as f32, 0.0],
                            3 => [0.0, (std::f64::consts::SQRT_2 * left) as f32],
                            4 => [left as f32, right as f32],
                            5 => [right as f32, left as f32],
                            6 => [
                                left as f32,
                                attack(age, instrument, 0, phase + std::f64::consts::FRAC_PI_2)
                                    as f32,
                            ],
                            _ => [
                                left as f32,
                                attack(age - 0.003, instrument, 1, phase + 0.7) as f32,
                            ],
                        }
                    });
                    let hits: Vec<_> = result
                        .percussion_decisions()
                        .iter()
                        .filter(|decision| decision.channel == instrument && decision.eligible)
                        .collect();
                    assert_eq!(hits.len(), 1, "rate={rate} instrument={instrument} phase={phase} layout={layout}; decisions={:?}", result.percussion_decisions());
                    assert!((0.0..=0.050).contains(&(hits[0].emission_time - 0.1)));
                    delays.push((hits[0].emission_time - 0.1) * 1000.0);
                }
            }
        }
    }
    delays.sort_by(f64::total_cmp);
    eprintln!(
        "96 production stereo HF cases, signal-to-emission ms: min={:.3}, median={:.3}, max={:.3}",
        delays[0], delays[48], delays[95]
    );
}

#[test]
fn equal_power_static_pan_and_polarity_do_not_add_energy_or_sharpness() {
    for instrument in [1, 2] {
        let mut records = Vec::new();
        for angle in [
            0.0,
            0.25,
            std::f64::consts::FRAC_PI_4,
            1.2,
            std::f64::consts::FRAC_PI_2,
        ] {
            let result = stream(48_000, 0.21, 137, fixed_floor_settings(), |time| {
                let value = attack(time - 0.1, instrument, 0, 0.7);
                [
                    (std::f64::consts::SQRT_2 * angle.cos() * value) as f32,
                    (std::f64::consts::SQRT_2 * angle.sin() * value) as f32,
                ]
            });
            let record = result
                .percussion_decisions()
                .iter()
                .find(|decision| decision.channel == instrument && decision.eligible)
                .expect("the equal-power hit must trigger")
                .clone();
            records.push(record);
        }
        for record in &records[1..] {
            assert!(
                (record.absolute_energy / records[0].absolute_energy - 1.0).abs() < 0.001,
                "static pan must preserve calibrated absolute energy"
            );
            assert!(
                (record.energy / records[0].energy - 1.0).abs() < 0.001,
                "static pan must not manufacture an attack-energy boost"
            );
            assert!(
                (record.sharpness - records[0].sharpness).abs() < 0.002,
                "the same attack must not become sharper simply by panning"
            );
            assert!((record.emission_time - records[0].emission_time).abs() <= 0.0011);
        }
    }
}

#[test]
fn slow_constant_power_panning_does_not_promote_a_sustained_tone() {
    for instrument in [1, 2] {
        for duration in [0.120, 0.600] {
            let result = stream(48_000, 1.0, 137, fixed_floor_settings(), |time| {
                let movement = ((time - 0.1) / duration).clamp(0.0, 1.0);
                let angle = movement * std::f64::consts::FRAC_PI_2;
                // The same held carrier moves slowly between the channels;
                // instantaneous total stereo power is exactly unchanged.
                let tone = 0.5 * carrier(time, instrument, 0, 0.7);
                [(tone * angle.cos()) as f32, (tone * angle.sin()) as f32]
            });
            assert!(
                result
                    .percussion_decisions()
                    .iter()
                    .filter(|decision| decision.channel == instrument)
                    .all(|decision| !decision.eligible),
                "slow pan duration={duration} instrument={instrument}: {:?}",
                result.percussion_decisions()
            );
        }
    }
}

#[test]
fn wide_stereo_event_features_and_emission_times_ignore_callback_partitioning() {
    let samples: Vec<_> = (0..43_200)
        .map(|sample| {
            let time = sample as f64 / 48_000.0;
            let age = if time < 0.6 { time - 0.1 } else { time - 0.6 };
            [attack(age, 1, 0, 0.3) as f32, attack(age, 1, 1, 1.7) as f32]
        })
        .collect();
    let run = |partition| {
        let mut visualizer = Visualizer::new();
        visualizer.set_detector_settings(fixed_floor_settings());
        for callback in samples.chunks(partition) {
            visualizer.consume_buf(callback, 1.0);
        }
        (
            visualizer.trigger_counts(),
            visualizer.percussion_decisions().clone(),
        )
    };
    let expected = run(48);
    assert!(expected.0.snare >= 2);
    for partition in [1, 137, 256, 4096] {
        assert_eq!(run(partition), expected, "partition={partition}");
    }
}

#[test]
fn independent_left_right_stationary_noise_does_not_become_percussion() {
    for gain in [0.003, 0.5, 1.0] {
        let mut states = [0x7392_c85a_u32, 0x19ab_45e7_u32];
        let result = stream(48_000, 3.0, 137, fixed_floor_settings(), |_| {
            std::array::from_fn(|side| {
                let state = &mut states[side];
                *state ^= *state << 13;
                *state ^= *state >> 17;
                *state ^= *state << 5;
                gain * (*state as f32 / u32::MAX as f32 * 2.0 - 1.0)
            })
        });
        let counts = result.trigger_counts();
        assert_eq!(
            [counts.kick, counts.snare, counts.hat],
            [0; 3],
            "independent steady stereo noise at gain={gain}; accepted={:?}",
            result
                .percussion_decisions()
                .iter()
                .filter(|decision| decision.eligible)
                .map(|decision| (
                    decision.channel,
                    decision.emission_time,
                    decision.sharpness,
                    decision.prominence,
                    decision.energy
                ))
                .collect::<Vec<_>>()
        );
    }
}
