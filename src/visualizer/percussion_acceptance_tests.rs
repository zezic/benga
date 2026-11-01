//! Acceptance fixtures use the complete production stream, including both FFT
//! branches and sample-clock event publication. These are acoustic assertions;
//! sharply plucked tonal sounds remain an explicitly measured ambiguity.

use super::*;

fn stream(
    rate: usize,
    fft: usize,
    seconds: f64,
    mut signal: impl FnMut(usize, f64) -> f64,
) -> Visualizer {
    let mut visualizer = Visualizer::new();
    visualizer.set_samplerate(rate);
    let mut settings = visualizer.detector_settings();
    settings.percussion.fft_size = fft;
    visualizer.set_detector_settings(settings);
    let mut callback = Vec::with_capacity(137);
    for n in 0..(seconds * rate as f64).round() as usize {
        let value = signal(n, n as f64 / rate as f64) as f32;
        callback.push([value, -value]);
        if callback.len() == 137 {
            visualizer.consume_buf(&callback, 1.0);
            callback.clear();
        }
    }
    visualizer.consume_buf(&callback, 1.0);
    visualizer
}

fn carrier(age: f64, channel: usize, phase: f64) -> f64 {
    let frequencies: &[f64] = match channel {
        0 => &[120.0],
        1 => &[2500.0, 3300.0, 4500.0, 6000.0],
        _ => &[7000.0, 9500.0, 13000.0],
    };
    frequencies
        .iter()
        .map(|f| (std::f64::consts::TAU * f * age + phase).sin())
        .sum::<f64>()
        / (frequencies.len() as f64).sqrt()
}

fn hit(age: f64, channel: usize, phase: f64) -> f64 {
    if age < 0.0 {
        return 0.0;
    }
    let decay = if channel == 0 { 0.060 } else { 0.025 };
    0.6 * (age / 0.001).min(1.0) * (-age / decay).exp() * carrier(age, channel, phase)
}

#[test]
fn canonical_high_frequency_attacks_emit_within_50ms_at_both_fast_resolutions() {
    let mut emission_delays = Vec::new();
    for rate in [44100, 48000, 96000] {
        for fft in [256, 512] {
            for channel in [1, 2] {
                for phase_index in 0..8 {
                    let phase = phase_index as f64 * std::f64::consts::TAU / 8.0;
                    let visualizer =
                        stream(rate, fft, 0.20, |_, at| hit(at - 0.1, channel, phase) * 0.5);
                    let decisions: Vec<_> = visualizer
                        .percussion_decisions()
                        .iter()
                        .filter(|d| d.channel == channel && d.eligible)
                        .collect();
                    assert_eq!(
                        decisions.len(),
                        1,
                        "rate={rate} fft={fft} channel={channel} phase={phase}; decisions={:?}",
                        visualizer.percussion_decisions()
                    );
                    let decision = decisions[0];
                    assert!(decision.provisional);
                    assert!(decision.sharpness_rank.is_none() && decision.energy_rank.is_none());
                    assert!((0.0..=0.050).contains(&(decision.emission_time - 0.1)));
                    emission_delays.push((decision.emission_time - 0.1) * 1000.0);
                    assert!(decision.onset_time <= decision.completion_time);
                    assert_eq!(decision.emission_time, decision.completion_time);
                }
            }
        }
    }
    emission_delays.sort_by(f64::total_cmp);
    eprintln!("96 production HF cases, signal-to-emission ms: min={:.3}, median={:.3}, p95={:.3}, max={:.3}",emission_delays[0],emission_delays[48],emission_delays[91],emission_delays[95]);
}

#[test]
fn coincident_kick_snare_and_hat_publish_independent_events() {
    let visualizer = stream(48000, 512, 0.25, |_, at| {
        let age = at - 0.1;
        (0..3).map(|channel| hit(age, channel, 0.7) * 0.65).sum()
    });
    for channel in 0..3 {
        let decisions: Vec<_> = visualizer
            .percussion_decisions()
            .iter()
            .filter(|d| d.channel == channel && d.eligible)
            .collect();
        assert_eq!(
            decisions.len(),
            1,
            "channel={channel}; {:?}",
            visualizer.percussion_decisions()
        );
        assert!(decisions[0].emission_time - 0.1 <= 0.05);
    }
    let counts = visualizer.trigger_counts();
    assert_eq!([counts.kick, counts.snare, counts.hat], [1, 1, 1]);
}

#[test]
fn repeated_identical_hits_survive_warmup_and_100th_percentile_ties() {
    let rate = 48000;
    let mut visualizer = Visualizer::new();
    let mut settings = visualizer.detector_settings();
    for channel in &mut settings.percussion.channels {
        channel.sharpness_percentile = 100.0;
        channel.energy_percentile = 100.0;
    }
    visualizer.set_detector_settings(settings);
    let initial = rate / 10;
    let spacing = rate / 2;
    let hits = 12;
    let mut callback = Vec::with_capacity(137);
    for n in 0..initial + spacing * hits {
        let value = if n >= initial {
            let age = ((n - initial) % spacing) as f64 / rate as f64;
            (0..3)
                .map(|channel| hit(age, channel, 0.7) * 0.65)
                .sum::<f64>() as f32
        } else {
            0.0
        };
        callback.push([value, -value]);
        if callback.len() == 137 {
            visualizer.consume_buf(&callback, 1.0);
            callback.clear();
        }
    }
    visualizer.consume_buf(&callback, 1.0);
    for channel in 0..3 {
        let records: Vec<_> = visualizer
            .percussion_decisions()
            .iter()
            .filter(|d| d.channel == channel)
            .collect();
        assert_eq!(records.len(), hits, "channel={channel}; {:?}", records);
        assert!(
            records.iter().all(|d| d.admitted && d.eligible),
            "channel={channel}; {:?}",
            records
        );
        assert!(records[..8]
            .iter()
            .all(|d| d.provisional && d.sharpness_rank.is_none() && d.energy_rank.is_none()));
        assert!(records[8..]
            .iter()
            .all(|d| !d.provisional && d.sharpness_rank.is_some() && d.energy_rank.is_some()));
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record.reference.count, index);
            assert_eq!(
                record.reference.sharpness_histogram.iter().sum::<u32>() as usize,
                index
            );
            assert_eq!(
                record.reference.energy_histogram.iter().sum::<u32>() as usize,
                index
            );
        }
        assert_eq!(visualizer.percussion_reference(channel).count, hits);
    }
}

#[test]
fn smooth_curved_notes_do_not_self_promote_but_fast_bass_plucks_are_ambiguous() {
    for exponent in [1.0, 2.0, 3.0] {
        let smooth = stream(48000, 512, 0.35, |_, at| {
            let age = at - 0.1;
            if age < 0.0 {
                return 0.0;
            }
            let attack = (age / 0.120).min(1.0).powf(exponent);
            0.6 * attack * (-age / 0.15).exp() * (std::f64::consts::TAU * 120.0 * age).sin()
        });
        assert!(
            smooth
                .percussion_decisions()
                .iter()
                .filter(|d| d.channel == 0)
                .all(|d| !d.eligible),
            "curved exponent={exponent}; {:?}",
            smooth.percussion_decisions()
        );
    }
    // A struck/plucked narrow bass tone with a 1 ms attack has the same early
    // regional envelope as this canonical kick. Percentiles cannot identify
    // the source instrument; this expected positive records that limitation.
    let pluck = stream(48000, 512, 0.25, |_, at| hit(at - 0.1, 0, 0.7));
    assert_eq!(
        pluck
            .percussion_decisions()
            .iter()
            .filter(|d| d.channel == 0 && d.eligible)
            .count(),
        1
    );
    // A smooth piano-like additive tone remains part of the onset population,
    // rather than becoming percussion simply because it has a relative rank.
    let piano = stream(48000, 512, 0.35, |_, at| {
        let age = at - 0.1;
        if age < 0.0 {
            return 0.0;
        }
        let envelope = (age / 0.060).min(1.0) * (-age / 0.2).exp();
        envelope
            * (0.4 * (std::f64::consts::TAU * 120.0 * age).sin()
                + 0.2 * (std::f64::consts::TAU * 240.0 * age).sin()
                + 0.1 * (std::f64::consts::TAU * 360.0 * age).sin())
    });
    assert!(piano
        .percussion_decisions()
        .iter()
        .filter(|d| d.channel == 0)
        .all(|d| !d.eligible));
}
