//! Production adaptive-detail comparison with deterministic screen-space motion.
//!
//! Run `cargo test --release measures_importance_weighted_thinning -- --ignored --nocapture`.
//! BENGA_THINNING_BENCH_DIR controls artifacts. Extents and GPU samples use the
//! existing BENGA_MOTION_BENCH_EXTENTS / BENGA_MOTION_BENCH_SAMPLES settings.

use std::{fs, hint::black_box, path::PathBuf, time::Instant};

use anyhow::{ensure, Context};
use serde_json::{json, Value};
use strum::EnumCount;

use crate::audio::timeline::AudioTime;

use super::super::{
    geometry::{Camera, Pose, Vec2, Vec3},
    model::{Model, ModelSet},
    showcase::{FocusView, SelectorView},
    Adjustment, BandRouting, CreaseThreshold, DetailThinning, Frame, Gains, LineInstance, Mixer,
    ModelComposite, ModelRuntime, ModelTint, MotionFade, MotionFadePower, MotionOpacity, Music,
    Palette, Property, PropertyGates, RibbonInstance, Rng, ShowcaseFrame, Style, ThemeId,
    BAND_COUNT,
};
use super::{extents, gpu::GpuBench, summary, timing_json};

#[derive(Clone, Copy)]
struct Motion {
    name: &'static str,
    /// Endpoint travel at physical 1080p and 60 FPS.
    dx: f32,
    zoom: f32,
}

const MOTIONS: [Motion; 4] = [
    Motion {
        name: "still",
        dx: 0.0,
        zoom: 1.0,
    },
    Motion {
        name: "mild_translation",
        dx: 15.0,
        zoom: 1.0,
    },
    Motion {
        name: "fast_translation",
        dx: 240.0,
        zoom: 1.0,
    },
    Motion {
        name: "zoomed_fast_translation",
        dx: 240.0,
        zoom: 2.0,
    },
];

fn gates() -> PropertyGates {
    let mut muted = [true; Property::COUNT];
    muted[Property::Line.index()] = false;
    PropertyGates {
        muted,
        soloed: [false; Property::COUNT],
    }
}

fn music() -> Music {
    Music {
        bands: [0.0; BAND_COUNT],
        raw_bands: [0.0; BAND_COUNT],
        power: 0.0,
        power_level: 0.0,
        bass: 0.0,
        bass_level: 0.0,
        kick: 0.0,
        kick_onset: 0.0,
        snare_onset: 0.0,
        drop_onset: 0.0,
    }
}

fn showcase() -> ShowcaseFrame {
    ShowcaseFrame {
        focus: FocusView {
            center: Vec3::default(),
            radius: 0.5,
            orbit_yaw: 0.0,
            orbit_pitch: 0.0,
            orbit_drift_yaw: 0.0,
            dolly_scale: 1.0,
            fov: 0.75,
            blend: 0.0,
            model_motion: 0.0,
            freeze_motion: false,
        },
        highlight_part: None,
        focus_child: None,
        mesh_attenuation: 1.0,
        slide_offset: Vec3::default(),
        selector: SelectorView::NONE,
    }
}

#[derive(Default)]
struct Output {
    lines: Vec<LineInstance>,
    ribbons: Vec<RibbonInstance>,
}

impl Output {
    fn frame(&self) -> Frame<'_> {
        Frame {
            lines: &[],
            model_lines: &self.lines,
            ribbons: &self.ribbons,
            model_composite: ModelComposite::Additive,
        }
    }

    fn triangle_area_px2(&self) -> f64 {
        self.ribbons
            .iter()
            .map(|ribbon| {
                let [ax, ay, bx, by] = ribbon.start;
                let [cx, cy] = ribbon.end;
                f64::from(((bx - ax) * (cy - ay) - (by - ay) * (cx - ax)).abs()) * 0.5
            })
            .sum()
    }
}

struct Workload {
    runtime: ModelRuntime,
    output: Output,
    frame: u64,
    fps: u32,
    width: u32,
    height: u32,
    motion: Motion,
    displacement: Vec2,
    setup_ms: f64,
}

impl Workload {
    fn new(
        model: &Model,
        width: u32,
        height: u32,
        fps: u32,
        motion: Motion,
        strength: f32,
    ) -> anyhow::Result<Self> {
        let setup_start = Instant::now();
        let mut runtime = ModelRuntime::new(
            model.name.clone(),
            model.wireframe(CreaseThreshold::default()),
            Pose::default(),
            Style::default(),
            ModelTint::solid(0.5),
            true,
            model.node_parts(super::super::model::SLIDE_NODE),
        );
        let setup_ms = setup_start.elapsed().as_secs_f64() * 1000.0;
        runtime.detail_thinning = DetailThinning::try_from(strength)?;
        let dx = motion.dx * (height as f32 / 1080.0) * (60.0 / fps as f32);
        let mut workload = Self {
            runtime,
            output: Output::default(),
            frame: 0,
            fps,
            width,
            height,
            motion,
            displacement: Vec2::new(dx, dx * 0.5),
            setup_ms,
        };
        // Establish one complete, zero-motion endpoint before the first sample.
        workload.update(AudioTime::ZERO, AudioTime::ZERO, 0.0, Vec2::default());
        Ok(workload)
    }

    fn update(&mut self, start: AudioTime, end: AudioTime, dt: f32, shake: Vec2) {
        self.output.lines.clear();
        self.output.ribbons.clear();
        self.runtime.update(
            dt,
            &music(),
            &Gains::default(),
            &BandRouting::default(),
            &Mixer::default(),
            &gates(),
            start,
            end,
            Camera::new(self.width as f32, self.height as f32, self.motion.zoom),
            &Palette::from_theme(ThemeId::default()),
            0.5,
            0.6,
            MotionFade {
                opacity: MotionOpacity::default(),
                power: MotionFadePower::default(),
            },
            (self.width.min(self.height) as f32 / 440.0).clamp(0.62, 2.6) * 1.4 * 0.6,
            &showcase(),
            0.0,
            shake,
            end.seconds_since(AudioTime::ZERO) as f32,
            &mut self.output.lines,
            &mut self.output.ribbons,
        );
    }

    fn advance(&mut self) {
        let start = AudioTime::ZERO.shifted(self.frame as f64 / f64::from(self.fps));
        self.frame += 1;
        let end = AudioTime::ZERO.shifted(self.frame as f64 / f64::from(self.fps));
        // The 64-frame warmup and even CPU sample counts finish on endpoint B.
        // First step stays on A, so onset captures both the baseline and attack.
        let shake = if self.frame % 2 == 0 {
            self.displacement
        } else {
            Vec2::default()
        };
        self.update(start, end, 1.0 / self.fps as f32, shake);
    }
}

fn heaviest_model(models: &mut ModelSet) -> anyhow::Result<(String, usize)> {
    let mut ranking = Vec::new();
    for _ in 0..models.position().1 {
        let model = models.current();
        ranking.push((
            model.id.clone(),
            model.wireframe(CreaseThreshold::default()).edges().len(),
        ));
        models.navigate(Adjustment::Increase);
    }
    ranking.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let (id, edges) = ranking.into_iter().next().context("no models loaded")?;
    for _ in 0..models.position().1 {
        if models.current().id == id {
            return Ok((id, edges));
        }
        models.navigate(Adjustment::Increase);
    }
    unreachable!("ranked model remains in the catalog")
}

#[test]
#[ignore = "requires GPU timestamps; writes production thinning measurements and previews"]
fn measures_importance_weighted_thinning() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var("BENGA_THINNING_BENCH_DIR")
            .unwrap_or_else(|_| "target/thinning-benchmark".into()),
    );
    fs::create_dir_all(&directory)?;
    let samples: usize = std::env::var("BENGA_MOTION_BENCH_SAMPLES")
        .unwrap_or_else(|_| "128".into())
        .parse()?;
    ensure!((1..=4096).contains(&samples), "samples must be in 1..=4096");
    let mut models = ModelSet::load(&mut Rng::new(0x5448_494e_4e49_4e47))?;
    let (model_id, raw_edges) = heaviest_model(&mut models)?;
    let model = models.current();
    eprintln!("Production thinning: {model_id}, {raw_edges} edges; {samples} samples/case");
    let mut cases: Vec<Value> = Vec::new();
    let mut adapters = Vec::new();
    for (width, height) in extents()? {
        let mut gpu = GpuBench::new(width, height)?;
        adapters.push(gpu.adapter_description());
        for fps in [60, 120] {
            for motion in MOTIONS {
                for (policy, strength) in [("off", 0.0), ("weighted", 1.0)] {
                    let mut workload = Workload::new(model, width, height, fps, motion, strength)?;
                    let initial_edges = workload.runtime.active_edges;
                    let mut onset_edges = Vec::new();
                    for index in 0..64 {
                        workload.advance();
                        if index < 8 {
                            onset_edges.push(workload.runtime.active_edges);
                        }
                    }
                    let mut cpu_ms = Vec::with_capacity(samples);
                    for _ in 0..samples {
                        let before = Instant::now();
                        black_box(&mut workload).advance();
                        cpu_ms.push(before.elapsed().as_secs_f64() * 1000.0);
                    }
                    // Freeze the same final B endpoint even for an odd sample request.
                    if workload.frame % 2 != 0 {
                        workload.advance();
                    }
                    let active_edges = workload.runtime.active_edges;
                    ensure!(
                        workload.runtime.current.len() == raw_edges,
                        "thinning must retain complete current geometry"
                    );
                    ensure!(
                        workload
                            .runtime
                            .history
                            .back()
                            .is_some_and(|pose| pose.lines.len() == raw_edges),
                        "thinning must retain complete motion history"
                    );
                    ensure!(
                        active_edges <= raw_edges,
                        "adaptive detail cannot add model edges"
                    );
                    if strength == 0.0 || motion.dx == 0.0 {
                        ensure!(
                            active_edges == raw_edges,
                            "off or settled still detail must retain every edge"
                        );
                    } else if motion.dx >= 240.0 {
                        ensure!(
                            active_edges < raw_edges && active_edges > 0,
                            "fast motion must reduce detail without hiding the model"
                        );
                    }
                    let timing = gpu.measure(workload.output.frame(), samples)?;
                    let gpu_stats = summary(&timing.gpu_ms);
                    let cpu_stats = summary(&cpu_ms);
                    let triangles = workload.output.ribbons.len() + workload.output.lines.len() * 2;
                    eprintln!("{width}x{height} {fps}Hz {:25} {policy:8} {active_edges}/{raw_edges} edges; CPU {:.3}ms GPU {:.3}/{:.3}ms median/p95",
                        motion.name,cpu_stats["median"].as_f64().unwrap(),
                        gpu_stats["median"].as_f64().unwrap(),gpu_stats["p95"].as_f64().unwrap());
                    gpu.write_preview(
                        workload.output.frame(),
                        &directory.join(format!(
                            "{width}x{height}-{fps}hz-{}-{policy}.png",
                            motion.name
                        )),
                    )?;
                    let mut result = json!({
                        "width":width,"height":height,"fps":fps,"motion":motion.name,"policy":policy,
                        "detail_strength":strength,"camera_zoom":motion.zoom,
                        "endpoint_displacement_px":[workload.displacement.x,workload.displacement.y],
                        "endpoint_speed_px_per_second":workload.displacement.length()*fps as f32,
                        "raw_edges":raw_edges,"initial_edges":initial_edges,"onset_first_eight_frames":onset_edges,
                        "active_edges":active_edges,"retained_fraction":active_edges as f64/raw_edges as f64,
                        "triangles":triangles,"ribbons":workload.output.ribbons.len(),
                        "overlapping_triangle_area_px2":workload.output.triangle_area_px2(),
                        "cpu_actual_update_ms":cpu_stats,"cpu_samples_ms":cpu_ms,
                        "batch_size":gpu.batch_size(),"timing":timing_json(&timing),
                        "cpu_runtime_setup_ms":workload.setup_ms,
                    });
                    // A stopped model must regain geometry gradually, while
                    // retaining the same fixed endpoint instead of jumping home.
                    let mut recovery_edges = Vec::new();
                    for _ in 0..fps / 2 {
                        let start = AudioTime::ZERO.shifted(workload.frame as f64 / f64::from(fps));
                        workload.frame += 1;
                        let end = AudioTime::ZERO.shifted(workload.frame as f64 / f64::from(fps));
                        workload.update(start, end, 1.0 / fps as f32, workload.displacement);
                        recovery_edges.push(workload.runtime.active_edges);
                    }
                    if strength > 0.0 && motion.dx >= 240.0 {
                        ensure!(
                            recovery_edges[0] < raw_edges,
                            "hidden geometry must return gradually"
                        );
                    }
                    ensure!(
                        recovery_edges.last() == Some(&raw_edges),
                        "detail must fully recover after stopping"
                    );
                    result["recovery_half_second_edges"] = json!(recovery_edges);
                    cases.push(result);
                }
            }
        }
    }
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec_pretty(&json!({
            "model":model_id,"raw_edges":raw_edges,"samples_per_case":samples,
            "crease_degrees":CreaseThreshold::default().degrees(),"adapters":adapters,"cases":cases,
            "method":"Production ModelRuntime::update, default importance metadata, Auto Detail strengths0 and1; fixed camera/model, no audio, no transition. Complete first endpoint recorded at audio time0;64 frames warmup, then timed CPU updates with reused output capacity. Alternating A/B screen offsets give constant displacement magnitude and finish at B. At120Hz offset is halved to preserve physical speed. GPU measures one frozen final frame through the real renderer, outside CPU update timing.",
            "timing_scope":"CPU actual update includes motion state/projection/history/adaptive thinning/ribbon construction, but excludes model loading, priority preparation, GPU upload and UI/audio. GPU pass excludes staging uploads, UI rasterization and presentation. These are component costs, not achieved application FPS.",
            "area_scope":"Sum of actual emitted ribbon triangle areas, including overlap and offscreen portions; an estimate of coverage work rather than measured fragments.",
        }))?,
    )?;
    eprintln!(
        "Saved production thinning comparison to {}",
        directory.display()
    );
    Ok(())
}
