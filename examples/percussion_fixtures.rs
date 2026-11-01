//! Reproducible production-stream feature/latency measurements; no audio devices.
use benga::visualizer::{PercussionDecision, Visualizer};
use serde_json::json;

fn run(
    rate: usize,
    fft: usize,
    channel: usize,
    phase: f64,
    gain: f32,
    rise: f64,
) -> serde_json::Value {
    let mut visualizer = Visualizer::new();
    visualizer.set_samplerate(rate);
    let mut settings = visualizer.detector_settings();
    settings.percussion.fft_size = fft;
    visualizer.set_detector_settings(settings);
    let start = 0.100;
    let mut records: Vec<PercussionDecision> = Vec::new();
    let mut last_id = None;
    // Irregular callback size deliberately differs from either analysis hop.
    let mut callback = Vec::with_capacity(137);
    for frame in 0..rate / 2 {
        let time = frame as f64 / rate as f64;
        let age = time - start;
        let value = if age >= 0.0 {
            let attack = (age / rise).min(1.0);
            let decay = (-age / if channel == 0 { 0.060 } else { 0.025 }).exp();
            let frequencies: &[f64] = match channel {
                0 => &[120.0],
                1 => &[2500.0, 3300.0, 4500.0, 6000.0],
                _ => &[7000.0, 9500.0, 13000.0],
            };
            let carrier = frequencies
                .iter()
                .map(|f| (std::f64::consts::TAU * f * age + phase).sin())
                .sum::<f64>()
                / (frequencies.len() as f64).sqrt();
            (0.6 * attack * decay * carrier) as f32
        } else {
            0.0
        };
        callback.push([value, -value]);
        if callback.len() == 137 || frame + 1 == rate / 2 {
            visualizer.consume_buf(&callback, gain);
            callback.clear();
            for decision in visualizer.percussion_decisions() {
                if decision.channel == channel && last_id.is_none_or(|id| decision.id > id) {
                    last_id = Some(decision.id);
                    records.push(decision.clone());
                }
            }
        }
    }
    let delays: Vec<_> = records
        .iter()
        .filter(|d| d.eligible)
        .map(|d| (d.emission_time - start) * 1000.0)
        .collect();
    json!({"sample_rate":rate,"fast_fft":fft,"channel":channel,"phase":phase,"gain":gain,"rise_ms":rise*1000.0,
        "known_signal_onset":start,"signal_to_emission_ms":delays,"candidates":records.len(),
        "admitted":records.iter().filter(|d| d.admitted).count(),"decisions":records})
}

fn main() -> anyhow::Result<()> {
    let cpu = cpu_time::ProcessTime::now();
    let mut cases = Vec::new();
    for rate in [44_100, 48_000, 96_000] {
        for fft in [256, 512] {
            for channel in 0..3 {
                for phase_index in 0..8 {
                    cases.push(run(
                        rate,
                        fft,
                        channel,
                        phase_index as f64 * std::f64::consts::TAU / 8.0,
                        0.5,
                        0.001,
                    ));
                }
            }
        }
    }
    for rise in [0.001, 0.015, 0.030, 0.060, 0.120] {
        for gain in [1.0, 0.5, 0.125, 0.031_622_78] {
            cases.push(run(48_000, 512, 0, 0.0, gain, rise));
        }
    }
    let report = json!({"contract":"docs/percussion-feature-contract.md","anti_phase_stereo":true,"callback_frames":137,"cpu_seconds":cpu.elapsed().as_secs_f64(),"cases":cases});
    serde_json::to_writer(std::io::stdout().lock(), &report)?;
    println!();
    Ok(())
}
