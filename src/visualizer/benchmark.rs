//! Repeatable model-render measurements, separate from audio and display pacing.
//!
//! Run: `cargo test --release measures_frozen_motion -- --ignored --nocapture --test-threads=1`
//! Optional environment: BENGA_MOTION_BENCH_DIR, BENGA_MOTION_BENCH_SAMPLES,
//! BENGA_MOTION_BENCH_MODEL, BENGA_MOTION_BENCH_EXTENTS (1920x1080,3840x2160),
//! BENGA_MOTION_BENCH_MOTIONS / BENGA_MOTION_BENCH_DETAILS (comma-separated names).
//! BENGA_MOTION_BENCH_BATCH_SIZE defaults to 1; use 8 to compare queued throughput.
//! JSON retains every sample; CSV gives summaries. PNG readbacks are untimed.

mod fixture;
mod fullscreen_capacity;
mod fullscreen_fixtures;
mod gpu;
mod pipeline;
mod poi_workload;
mod transition;
mod transparent_pixels;
mod zoom_coverage;
mod zoom_workload;

use std::{
    fs::{self, File},
    hint::black_box,
    io::Write,
    path::PathBuf,
    time::Instant,
};

use anyhow::{ensure, Context};
use serde_json::{json, Value};

use super::{CreaseThreshold, Frame, ModelComposite};
use fixture::{Detail, FrozenOutput, Motion};
use gpu::{GpuBench, Timings};

fn summary(samples: &[f64]) -> Value {
    assert!(!samples.is_empty());
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| {
        sorted[((sorted.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
    };
    json!({
        "min": sorted[0], "median": percentile(0.5),
        "p95": percentile(0.95), "max": sorted[sorted.len() - 1],
    })
}

fn selected(variable: &str, name: &str) -> bool {
    std::env::var(variable)
        .map(|filter| filter.split(',').any(|item| item.trim() == name))
        .unwrap_or(true)
}

fn extents() -> anyhow::Result<Vec<(u32, u32)>> {
    std::env::var("BENGA_MOTION_BENCH_EXTENTS")
        .unwrap_or_else(|_| "1920x1080,3840x2160".into())
        .split(',')
        .map(|extent| {
            let (width, height) = extent
                .trim()
                .split_once('x')
                .context("extent must be WIDTHxHEIGHT")?;
            let (width, height) = (width.parse::<u32>()?, height.parse::<u32>()?);
            ensure!(width > 0 && height > 0, "extent must be nonzero");
            Ok((width, height))
        })
        .collect()
}

fn timing_json(timing: &Timings) -> Value {
    json!({
        "gpu_pass_ms": summary(&timing.gpu_ms),
        "cpu_upload_encode_submit_ms": summary(&timing.cpu_submit_ms),
        "batch_wall_ms_per_frame": timing.wall_ms_per_frame,
        "gpu_batch_span_ms_per_frame": summary(&timing.gpu_batch_ms_per_frame),
        "gpu_batch_span_samples_ms_per_frame": timing.gpu_batch_ms_per_frame,
        "gpu_samples_ms": timing.gpu_ms,
        "cpu_submit_samples_ms": timing.cpu_submit_ms,
    })
}

#[test]
#[ignore = "requires a GPU with timestamp queries; writes benchmark artifacts"]
fn measures_frozen_motion() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var("BENGA_MOTION_BENCH_DIR")
            .unwrap_or_else(|_| "target/motion-benchmark".into()),
    );
    fs::create_dir_all(&directory)?;
    let samples: usize = std::env::var("BENGA_MOTION_BENCH_SAMPLES")
        .unwrap_or_else(|_| "64".into())
        .parse()?;
    ensure!(
        samples > 0 && samples <= 4096,
        "samples must be in 1..=4096"
    );
    let fixtures = fixture::load_ranked()?;
    let ranking: Vec<_> = fixtures
        .iter()
        .map(|model| {
            json!({
                "id": model.id, "vertices": model.vertex_count(), "edges": model.edge_count(),
            })
        })
        .collect();
    let model = if let Ok(id) = std::env::var("BENGA_MOTION_BENCH_MODEL") {
        fixtures
            .iter()
            .find(|model| model.id == id)
            .context("requested model not found")?
    } else {
        fixtures.first().context("no models loaded")?
    };
    eprintln!(
        "Frozen model: {} — {} vertices, {} edges; crease {} degrees; {} samples/case",
        model.id,
        model.vertex_count(),
        model.edge_count(),
        CreaseThreshold::default().degrees(),
        samples
    );
    eprintln!(
        "Top models: {}",
        serde_json::to_string(&ranking[..ranking.len().min(5)])?
    );
    let mut csv = File::create(directory.join("summary.csv"))?;
    writeln!(csv, "width,height,motion,detail,retained_edges,triangles,overlapping_triangle_area_px2,cpu_project_median_ms,cpu_build_median_ms,cpu_build_p95_ms,cpu_submit_median_ms,gpu_median_ms,gpu_p95_ms,gpu_max_ms,batch_wall_ms_per_frame")?;
    let mut cases = Vec::new();
    let mut baselines = Vec::new();
    let mut adapters = Vec::new();
    let mut repeats = Vec::new();
    let motions: Vec<_> = Motion::ALL
        .into_iter()
        .filter(|motion| selected("BENGA_MOTION_BENCH_MOTIONS", motion.name()))
        .collect();
    let details: Vec<_> = Detail::ALL
        .into_iter()
        .filter(|detail| selected("BENGA_MOTION_BENCH_DETAILS", detail.name()))
        .collect();
    ensure!(
        !motions.is_empty() && !details.is_empty(),
        "motion/detail filter matched nothing"
    );
    for (width, height) in extents()? {
        let mut gpu = GpuBench::new(width, height)?;
        let adapter = gpu.adapter_description();
        eprintln!("{adapter}; measurement batch size {}", gpu.batch_size());
        adapters.push(adapter);
        let empty = Frame {
            lines: &[],
            model_lines: &[],
            ribbons: &[],
            model_composite: ModelComposite::Additive,
            model_edge_capacity: 0,
            gpu_active: false,
            gpu_models: &[],
        };
        baselines.push(json!({"width":width,"height":height,"timing":timing_json(&gpu.measure(empty, samples)?)}));
        for &motion in &motions {
            let mut pair = model.prepare(width, height, motion);
            // Projection of both endpoints is intentionally independent of the
            // emission variants. Model loading is excluded.
            let mut projection_ms = Vec::new();
            for index in 0..24 {
                let before = Instant::now();
                model.reproject(width, height, black_box(motion), black_box(&mut pair));
                if index >= 8 {
                    projection_ms.push(before.elapsed().as_secs_f64() * 1000.0);
                }
            }
            let projection = summary(&projection_ms);
            for &detail in &details {
                let mut output = FrozenOutput::default();
                for _ in 0..8 {
                    pair.build(detail, &mut output);
                }
                let mut build_ms = Vec::with_capacity(samples);
                for _ in 0..samples {
                    let before = Instant::now();
                    pair.build(black_box(detail), black_box(&mut output));
                    build_ms.push(before.elapsed().as_secs_f64() * 1000.0);
                }
                let build = summary(&build_ms);
                let timing = gpu.measure(output.frame(ModelComposite::Additive), samples)?;
                let gpu_summary = summary(&timing.gpu_ms);
                let submit = summary(&timing.cpu_submit_ms);
                let triangles = output.ribbons.len() + output.lines.len() * 2;
                let area = output.triangle_area_px2();
                eprintln!("{width}x{height} {:16} {:21} edges {:6} tris {:7} CPU {:6.3} ms GPU {:7.3} / {:7.3} ms median/p95",
                    motion.name(),detail.name(),output.retained_edges,triangles,build["median"].as_f64().unwrap(),
                    gpu_summary["median"].as_f64().unwrap(),gpu_summary["p95"].as_f64().unwrap());
                writeln!(csv,"{width},{height},{},{},{},{},{:.2},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
                    motion.name(),detail.name(),output.retained_edges,triangles,area,
                    projection["median"],build["median"],build["p95"],submit["median"],
                    gpu_summary["median"],gpu_summary["p95"],gpu_summary["max"],timing.wall_ms_per_frame)?;
                csv.flush()?;
                gpu.write_preview(
                    output.frame(ModelComposite::Additive),
                    &directory.join(format!(
                        "{width}x{height}-{}-{}.png",
                        motion.name(),
                        detail.name()
                    )),
                )?;
                cases.push(json!({
                    "width":width,"height":height,"motion":motion.name(),"detail":detail.name(),
                    "retained_edges":output.retained_edges,"triangles":triangles,"ribbons":output.ribbons.len(),
                    "sharp_lines":output.lines.len(),"overlapping_triangle_area_px2":area,
                    "line_width_px":pair.line_width,"mean_endpoint_displacement_px":pair.mean_displacement_px,
                    "max_endpoint_displacement_px":pair.max_displacement_px,
                    "first_pose":format!("{:?}",pair.first_pose),"last_pose":format!("{:?}",pair.last_pose),
                    "fixture_settings":pair.settings(),"batch_size":gpu.batch_size(),
                    "cpu_prepare_ms":projection,"cpu_build_ms":build,"cpu_build_samples_ms":build_ms,
                    "timing":timing_json(&timing),
                }));
            }
        }
        // Repeat the final, expensive motion after the matrix to expose drift.
        let motion = *motions.last().unwrap();
        let pair = model.prepare(width, height, motion);
        let mut output = FrozenOutput::default();
        pair.build(Detail::Full, &mut output);
        repeats.push(
            json!({"width":width,"height":height,"motion":motion.name(),"detail":"full_blur",
            "timing":timing_json(&gpu.measure(output.frame(ModelComposite::Additive),samples)?)}),
        );
    }
    let report = json!({
        "schema_version":1,"model":model.id,"ranking":ranking,"adapters":adapters,
        "samples_per_case":samples,"crease_degrees":CreaseThreshold::default().degrees(),
        "method":"Frozen two-pose model geometry; production camera/sweep/shader/blend; offscreen Bgra8Unorm; configurable batch size1..8 (default1);16 warmup frames per case; no audio, VSync, animated UI or adaptive quality. Sampling completes before counter resolve to work around Metal/wgpu issue9414.",
        "timing_scope":"GPU render pass includes cached-background composite and model only, excludes staging uploads, Vello UI rasterization and presentation. Batch wall includes upload/encode/submit, synchronization and query readback. CPU prepare reuses allocations for two endpoint projections and edge materialization, excluding model loading. CPU build reuses triangle capacity. No metric alone predicts application FPS.",
        "area_scope":"Sum of emitted triangle areas before viewport clipping; overlapping fragments count repeatedly. Sharp uses padded axis-aligned rectangles; coverage/alpha is not subtracted.",
        "queueing_scope":"Single-frame batches isolate pass execution. Queued pass vertex/fragment stages may overlap, making individual pass latencies exceed sustainable per-frame throughput. For batch>1 inspect GPU batch span/frame and batch wall separately; do not infer FPS from individual pass latency.",
        "frame_budgets_ms":{"60":1000.0/60.0,"120":1000.0/120.0,"144":1000.0/144.0,"165":1000.0/165.0,"240":1000.0/240.0,"30_emergency":1000.0/30.0},
        "empty_baselines":baselines,"cases":cases,"end_repeats":repeats,
    });
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    eprintln!(
        "Saved {} cases and frozen previews to {}",
        report["cases"].as_array().unwrap().len(),
        directory.display()
    );
    Ok(())
}

#[test]
fn benchmark_percentiles_use_nearest_rank() {
    let values: Vec<_> = (1..=100).rev().map(f64::from).collect();
    let stats = summary(&values);
    assert_eq!(stats["median"], 50.0);
    assert_eq!(stats["p95"], 95.0);
    assert_eq!(stats["max"], 100.0);
}
