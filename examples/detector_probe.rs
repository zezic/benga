//! Stream interleaved f32le stereo PCM through the production audio detectors.
//!
//! ffmpeg -i track.ogg -f f32le -ac 2 -ar 48000 - |
//!   cargo run --release --example detector_probe -- --gain-db -6 --label track
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    time::Instant,
};

use anyhow::{bail, Context};
use benga::visualizer::{DebugSnapshot, Visualizer};
use serde_json::{json, Value};

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}

fn quantile(values: &[f32], fraction: f32) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    Some(sorted[((sorted.len() - 1) as f32 * fraction).round() as usize])
}

fn summary(times: &[f64], duration: f64) -> Value {
    let intervals: Vec<_> = times.windows(2).map(|p| p[1] - p[0]).collect();
    let intervals_f32: Vec<_> = intervals.iter().map(|v| *v as f32).collect();
    json!({
        "count": times.len(), "per_second": times.len() as f64 / duration.max(0.001),
        "interval_p10": quantile(&intervals_f32, 0.1),
        "interval_p50": quantile(&intervals_f32, 0.5),
        "interval_p90": quantile(&intervals_f32, 0.9),
        "interval_under_50ms": intervals.iter().filter(|&&v| v < 0.05).count(),
        "interval_under_100ms": intervals.iter().filter(|&&v| v < 0.1).count(),
        "interval_under_150ms": intervals.iter().filter(|&&v| v < 0.15).count(),
        "interval_under_200ms": intervals.iter().filter(|&&v| v < 0.2).count(),
        "first_seconds": times.first(), "last_seconds": times.last(),
        "times": times,
    })
}

fn counts(snapshot: &DebugSnapshot) -> [u32; 4] {
    [
        snapshot.kick_hits,
        snapshot.snare_hits,
        snapshot.hat_hits,
        snapshot.drop_hits,
    ]
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help") {
        println!("detector_probe [--sample-rate 48000] [--gain-db -6] [--fft-size 1024] [--percussion-fft-size 512] [--label NAME] [--trace PATH] [--block-frames N] [--max-seconds N]\nReads stereo interleaved f32le PCM from stdin; writes one JSON summary. Trace is newline-delimited callback diagnostics plus immutable candidate JSON.");
        return Ok(());
    }
    for pair in args.chunks(2) {
        if pair.len() != 2 {
            bail!("{} requires a value", pair[0]);
        }
        if ![
            "--sample-rate",
            "--gain-db",
            "--fft-size",
            "--percussion-fft-size",
            "--label",
            "--trace",
            "--block-frames",
            "--max-seconds",
        ]
        .contains(&pair[0].as_str())
        {
            bail!("unknown option {}; use --help", pair[0]);
        }
    }
    let sample_rate: usize = option(&args, "--sample-rate").unwrap_or("48000").parse()?;
    if !(8_000..=384_000).contains(&sample_rate) {
        bail!("sample rate must be in 8000..=384000");
    }
    let gain_db: f32 = option(&args, "--gain-db").unwrap_or("-6").parse()?;
    if !gain_db.is_finite() {
        bail!("gain must be finite");
    }
    let gain = benga::db_to_multiplier(gain_db);
    let max_seconds: f64 = option(&args, "--max-seconds").unwrap_or("inf").parse()?;
    if max_seconds.is_nan() || max_seconds <= 0.0 {
        bail!("max seconds must be positive");
    }
    let mut visualizer = Visualizer::new();
    visualizer.set_samplerate(sample_rate);
    let fft_size: Option<usize> = option(&args, "--fft-size").map(str::parse).transpose()?;
    if let Some(size) = fft_size {
        if !(256..=8192).contains(&size) || !size.is_power_of_two() {
            bail!("FFT size must be a power of two in 256..=8192");
        }
        visualizer.set_analysis_fft_size(size);
    }
    if let Some(size) = option(&args, "--percussion-fft-size") {
        let size: usize = size.parse()?;
        if ![256, 512].contains(&size) {
            bail!("percussion FFT must be 256 or 512 samples at 48 kHz");
        }
        let mut settings = visualizer.detector_settings();
        settings.percussion.fft_size = size;
        visualizer.set_detector_settings(settings);
    }
    let hop_frames = (visualizer.hop_seconds() * sample_rate as f32).round() as usize;
    let block_frames: usize = option(&args, "--block-frames")
        .map(str::parse)
        .transpose()?
        .unwrap_or(hop_frames);
    if block_frames == 0 {
        bail!("block frames must be positive");
    }
    let mut trace = option(&args, "--trace")
        .map(std::fs::File::create)
        .transpose()?
        .map(io::BufWriter::new);
    let mut input = io::stdin().lock();
    let mut bytes = vec![0_u8; block_frames * 8];
    let mut frames = Vec::with_capacity(block_frames);
    let mut processed = 0usize;
    let mut previous_counts = [0; 4];
    let mut events: [Vec<f64>; 4] = std::array::from_fn(|_| Vec::new());
    let mut event_rms: [Vec<f32>; 4] = std::array::from_fn(|_| Vec::new());
    let mut gap_entries = Vec::new();
    let mut was_armed = false;
    let mut rms_db = Vec::new();
    let mut shape_distance = Vec::new();
    let mut trend_change = Vec::new();
    let mut changed_fraction = Vec::new();
    let mut armed_hops = 0usize;
    let mut gate_hops = [0usize; 3];
    let mut peaks = [0.0_f32; 3];
    let started = Instant::now();
    let cpu_started = cpu_time::ProcessTime::now();
    let mut last_decision = [None; 3];
    let mut candidate_counts = [0_u64; 3];
    let mut admission_counts = [0_u64; 3];
    let mut provisional_counts = [0_u64; 3];
    let mut reason_counts: [BTreeMap<String, u64>; 3] = std::array::from_fn(|_| BTreeMap::new());
    let mut feature_delays: [Vec<f32>; 3] = std::array::from_fn(|_| Vec::new());
    loop {
        let mut used = 0;
        while used < bytes.len() {
            let read = input.read(&mut bytes[used..])?;
            if read == 0 {
                break;
            }
            used += read;
        }
        if used == 0 {
            break;
        }
        if used % 8 != 0 {
            bail!("input ends in an incomplete stereo f32 frame");
        }
        frames.clear();
        for frame in bytes[..used].chunks_exact(8) {
            let left = f32::from_le_bytes(frame[..4].try_into().unwrap());
            let right = f32::from_le_bytes(frame[4..].try_into().unwrap());
            if !left.is_finite() || !right.is_finite() {
                bail!("PCM input contains a non-finite sample");
            }
            frames.push([left, right]);
        }
        // Bound telemetry polling independently of arbitrary capture callbacks.
        // At a minimum 0.5 ms hop, 10 ms contains at most 60 channel starts,
        // safely below the 256 immutable-record capacity.
        for chunk in frames.chunks((sample_rate / 100).max(1)) {
            visualizer.consume_buf(chunk, gain);
            // Immutable production records preserve every percussion decision and
            // actual emission time even when callbacks span multiple analysis hops.
            for decision in visualizer.percussion_decisions() {
                let index = decision.channel;
                if last_decision[index].is_some_and(|id| decision.id <= id) {
                    continue;
                }
                last_decision[index] = Some(decision.id);
                candidate_counts[index] += 1;
                admission_counts[index] += u64::from(decision.admitted);
                provisional_counts[index] += u64::from(decision.eligible && decision.provisional);
                *reason_counts[index]
                    .entry(decision.reason.label().to_owned())
                    .or_default() += 1;
                if decision.eligible {
                    events[index].push(decision.emission_time);
                    event_rms[index].push(decision.mix_rms_db);
                    feature_delays[index]
                        .push(((decision.emission_time - decision.onset_time) * 1000.0) as f32);
                }
                if let Some(trace) = &mut trace {
                    serde_json::to_writer(
                        &mut *trace,
                        &json!({"type":"candidate", "decision":decision}),
                    )?;
                    writeln!(trace)?;
                }
            }
        }
        processed += frames.len();
        let at = processed as f64 / sample_rate as f64;
        let snapshot = visualizer.debug_snapshot();
        let current_counts = counts(&snapshot);
        for index in 3..4 {
            for _ in previous_counts[index]..current_counts[index] {
                events[index].push(at);
                event_rms[index].push(snapshot.drop_status.rms_db);
            }
        }
        let armed = snapshot.drop_onset.armed;
        if armed && !was_armed {
            gap_entries.push(at);
        }
        was_armed = armed;
        rms_db.push(snapshot.drop_status.rms_db);
        shape_distance.push(snapshot.drop_status.shape_distance);
        trend_change.push(snapshot.drop_status.trend_change);
        changed_fraction.push(snapshot.drop_status.changed_fraction);
        armed_hops += usize::from(armed);
        let flux = [snapshot.kick_flux, snapshot.snare_flux, snapshot.hat_flux];
        for index in 0..3 {
            peaks[index] = peaks[index].max(flux[index].flux);
            gate_hops[index] += usize::from(flux[index].mix_gate);
        }
        if let Some(trace) = &mut trace {
            let row = json!({
                "at": at, "rms_db": snapshot.drop_status.rms_db,
                "envelope_db": snapshot.drop_status.envelope_db, "armed": armed,
                "counts": current_counts,
                "flux": flux.map(|f| f.flux), "threshold": flux.map(|f| f.threshold),
                "sharpness": flux.map(|f| f.sharpness),
                "coverage": flux.map(|f| f.coverage), "mix_gate": flux.map(|f| f.mix_gate),
                "bands": snapshot.bands.map(|b| b.raw),
                "shape": snapshot.drop_status.shape,
                "reference_shape": snapshot.drop_status.reference_shape,
                "shape_distance": snapshot.drop_status.shape_distance,
                "trend_change": snapshot.drop_status.trend_change,
                "changed_fraction": snapshot.drop_status.changed_fraction,
                "energy_delta_db": snapshot.drop_status.energy_delta_db,
                "bass_hit": snapshot.drop_status.bass_hit,
                "hit_threshold": snapshot.drop_status.hit_threshold,
                "quiet_progress": snapshot.drop_status.quiet_progress,
            });
            serde_json::to_writer(&mut *trace, &row)?;
            writeln!(trace)?;
        }
        previous_counts = current_counts;
        if at >= max_seconds {
            break;
        }
    }
    if let Some(trace) = &mut trace {
        trace.flush()?;
    }
    let duration = processed as f64 / sample_rate as f64;
    let mut detectors = BTreeMap::new();
    for (index, name) in ["kick", "snare", "hat", "drop"].into_iter().enumerate() {
        let mut values = summary(&events[index], duration);
        values["event_rms_db_min"] = json!(quantile(&event_rms[index], 0.0));
        values["event_rms_db_p10"] = json!(quantile(&event_rms[index], 0.1));
        values["event_rms_db_p50"] = json!(quantile(&event_rms[index], 0.5));
        if index < 3 {
            values["candidates"] = json!(candidate_counts[index]);
            values["admitted"] = json!(admission_counts[index]);
            values["provisional_triggers"] = json!(provisional_counts[index]);
            values["decision_reasons"] = json!(reason_counts[index]);
            values["onset_to_emission_ms_p50"] = json!(quantile(&feature_delays[index], 0.5));
            values["onset_to_emission_ms_p99"] = json!(quantile(&feature_delays[index], 0.99));
            values["onset_to_emission_ms_max"] = json!(quantile(&feature_delays[index], 1.0));
        }
        detectors.insert(name, values);
    }
    let report = json!({
        "label": option(&args, "--label").unwrap_or("stdin"),
        "sample_rate": sample_rate, "gain_db": gain_db, "duration": duration,
        "fft_size": fft_size,
        "hop_frames": hop_frames, "block_frames": block_frames,
        "analysis_hops": visualizer.analysis_revision(),
        "wall_seconds": started.elapsed().as_secs_f64(),
        "telemetry_poll_frames_max": (sample_rate / 100).max(1),
        "cpu_scope": "production analysis plus probe telemetry/JSON; excludes ffmpeg decoding",
        "cpu_seconds": cpu_started.elapsed().as_secs_f64(),
        "percussion_settings": visualizer.detector_settings().percussion,
        "percussion_support_seconds": visualizer.percussion_support_seconds(),
        "percussion_timestamp_semantics": "actual sample-clock emission; onset_to_emission excludes unknown source-onset to front-end detection delay",
        "rms_db_p10": quantile(&rms_db, 0.1), "rms_db_p50": quantile(&rms_db, 0.5),
        "rms_db_p90": quantile(&rms_db, 0.9),
        "shape_distance_p50": quantile(&shape_distance, 0.5),
        "shape_distance_p90": quantile(&shape_distance, 0.9),
        "shape_distance_p99": quantile(&shape_distance, 0.99),
        "trend_change_p90": quantile(&trend_change, 0.9),
        "changed_fraction_p90": quantile(&changed_fraction, 0.9),
        "armed_fraction": armed_hops as f32 / rms_db.len().max(1) as f32,
        "mix_gate_fraction": gate_hops.map(|h| h as f32 / rms_db.len().max(1) as f32),
        "flux_max": peaks, "gap_entries": gap_entries, "detectors": detectors,
    });
    serde_json::to_writer(io::stdout().lock(), &report).context("write detector report")?;
    println!();
    Ok(())
}
