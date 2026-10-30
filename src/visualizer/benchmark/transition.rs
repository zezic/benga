//! Production scene-transition snapshots and GPU comparison against global alpha.
//!
//! Run `cargo test --release measures_edge_exchange_transition -- --ignored --nocapture`.
//! BENGA_TRANSITION_BENCH_DIR controls artifacts; BENGA_MOTION_BENCH_EXTENTS and
//! BENGA_MOTION_BENCH_SAMPLES reuse the frozen-motion benchmark controls.

use std::{fs, path::PathBuf};

use anyhow::ensure;
use serde_json::json;
use strum::EnumCount;

use crate::audio::timeline::AudioTime;

use super::super::{
    geometry::{Camera, Pose, Vec2, Vec3},
    model::{Model, ModelSet},
    showcase::{FocusView, SelectorView},
    smoothstep, Adjustment, BandRouting, CreaseThreshold, Frame, Gains, LineInstance, Mixer,
    ModelComposite, ModelMotion, ModelRuntime, ModelTint, MotionFade, MotionFadePower,
    MotionOpacity, Music, Palette, Property, PropertyGates, RibbonInstance, Rng, Scene,
    ShowcaseFrame, Style, StyleTuning, ThemeId, BAND_COUNT,
};
use super::{extents, gpu::GpuBench, timing_json};

#[derive(Default)]
struct Output {
    lines: Vec<LineInstance>,
    ribbons: Vec<RibbonInstance>,
    active_edges: usize,
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

    /// Global alpha multiplied the same straight-alpha endpoint colors before
    /// sweep construction. Multiplication commutes with interpolation/falloff,
    /// so scaling here reconstructs the old pixels without changing geometry.
    fn scale_since(&mut self, line_start: usize, ribbon_start: usize, alpha: f32) {
        for line in &mut self.lines[line_start..] {
            line.start_color[3] *= alpha;
            line.end_color[3] *= alpha;
        }
        for ribbon in &mut self.ribbons[ribbon_start..] {
            ribbon.start_color[3] *= alpha;
            ribbon.end_color[3] *= alpha;
            ribbon.third_color[3] *= alpha;
        }
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

fn gates() -> PropertyGates {
    let mut muted = [true; Property::COUNT];
    muted[Property::Line.index()] = false;
    muted[Property::Crossfade.index()] = false;
    PropertyGates {
        muted,
        soloed: [false; Property::COUNT],
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
            model_motion: 1.0,
            freeze_motion: false,
        },
        highlight_part: None,
        focus_child: None,
        mesh_attenuation: 1.0,
        slide_offset: Vec3::default(),
        selector: SelectorView::NONE,
    }
}

fn select<'a>(models: &'a mut ModelSet, id: &str) -> &'a Model {
    for _ in 0..models.position().1 {
        if models.current().id == id {
            return models.current();
        }
        models.navigate(Adjustment::Increase);
    }
    panic!("ranked model {id} disappeared from the catalog");
}

fn scene_and_baseline(models: &mut ModelSet, ids: &[String; 2]) -> (Scene, [ModelRuntime; 2]) {
    let mut scene = Scene::new();
    scene.detail_thinning = super::super::DetailThinning::try_from(0.0).unwrap();
    let mut runtimes = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let model = select(models, id);
        let accent = if index == 0 { 0.15 } else { 0.75 };
        if index == 0 {
            scene.show(
                model,
                CreaseThreshold::default(),
                ModelMotion::Swept,
                accent,
                &StyleTuning::default(),
            );
        } else {
            scene.switch(
                model,
                CreaseThreshold::default(),
                ModelMotion::Swept,
                accent,
            );
        }
        runtimes.push(ModelRuntime::new(
            model.name.clone(),
            model.wireframe(CreaseThreshold::default()),
            Pose {
                yaw: std::f32::consts::TAU * 0.25,
                ..Pose::default()
            },
            Style::default(),
            ModelTint::solid(accent),
            true,
            model.node_parts(super::super::model::SLIDE_NODE),
        ));
    }
    (scene, runtimes.try_into().ok().unwrap())
}

fn step_scene(scene: &mut Scene, width: u32, height: u32, end: AudioTime, shake: Vec2) -> Output {
    let mut output = Output::default();
    scene.update(
        0.0,
        &music(),
        &Gains::default(),
        &BandRouting::default(),
        &Mixer::default(),
        &gates(),
        AudioTime::ZERO,
        end,
        Camera::new(width as f32, height as f32, 1.0),
        &Palette::from_theme(ThemeId::default()),
        0.5,
        0.6,
        MotionFade {
            opacity: MotionOpacity::default(),
            power: MotionFadePower::default(),
        },
        (width.min(height) as f32 / 440.0).clamp(0.62, 2.6) * 1.4 * 0.6,
        &showcase(),
        0.0,
        shake,
        0.0,
        &mut output.lines,
        &mut output.ribbons,
    );
    output.active_edges = scene.current.as_ref().map_or(0, |model| model.active_edges)
        + scene
            .outgoing
            .as_ref()
            .map_or(0, |model| model.active_edges);
    output
}

fn step_baseline(
    runtimes: &mut [ModelRuntime; 2],
    width: u32,
    height: u32,
    end: AudioTime,
    shake: Vec2,
    progress: f32,
) -> Output {
    let mut output = Output::default();
    for (runtime, alpha) in runtimes.iter_mut().zip([1.0 - progress, progress]) {
        runtime.detail_thinning = super::super::DetailThinning::try_from(0.0).unwrap();
        runtime.edge_dissolve.show_all();
        let (line_start, ribbon_start) = (output.lines.len(), output.ribbons.len());
        runtime.update(
            0.0,
            &music(),
            &Gains::default(),
            &BandRouting::default(),
            &Mixer::default(),
            &gates(),
            AudioTime::ZERO,
            end,
            Camera::new(width as f32, height as f32, 1.0),
            &Palette::from_theme(ThemeId::default()),
            0.5,
            0.6,
            MotionFade {
                opacity: MotionOpacity::default(),
                power: MotionFadePower::default(),
            },
            (width.min(height) as f32 / 440.0).clamp(0.62, 2.6) * 1.4 * 0.6,
            &showcase(),
            0.0,
            shake,
            0.0,
            &mut output.lines,
            &mut output.ribbons,
        );
        output.active_edges += runtime.active_edges;
        output.scale_since(line_start, ribbon_start, alpha);
    }
    output
}

#[test]
#[ignore = "requires GPU timestamps; writes production transition previews and comparisons"]
fn measures_edge_exchange_transition() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var("BENGA_TRANSITION_BENCH_DIR")
            .unwrap_or_else(|_| "target/transition-benchmark".into()),
    );
    fs::create_dir_all(&directory)?;
    let samples: usize = std::env::var("BENGA_MOTION_BENCH_SAMPLES")
        .unwrap_or_else(|_| "64".into())
        .parse()?;
    ensure!((1..=4096).contains(&samples), "samples must be in 1..=4096");
    let mut models = ModelSet::load(&mut Rng::new(0x5452_414e_5349_5449))?;
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
    ensure!(
        ranking.len() >= 2,
        "transition benchmark requires two models"
    );
    let ids = [ranking[0].0.clone(), ranking[1].0.clone()];
    let largest_edges = ranking[0].1;
    let full_pair_edges = ranking[0].1 + ranking[1].1;
    eprintln!(
        "Transition: {} ({} edges) -> {} ({} edges)",
        ids[0], ranking[0].1, ids[1], ranking[1].1
    );
    let mut cases = Vec::new();
    let mut adapters = Vec::new();
    for (width, height) in extents()? {
        let mut gpu = GpuBench::new(width, height)?;
        adapters.push(gpu.adapter_description());
        for translating in [false, true] {
            let motion = if translating { "translation" } else { "still" };
            for phase in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
                // Static snapshots show the whole exchange; midpoint isolates
                // the expensive moving-transition comparison without repetition.
                if translating && phase != 0.5 {
                    continue;
                }
                let (mut scene, mut baseline) = scene_and_baseline(&mut models, &ids);
                let _ = step_scene(&mut scene, width, height, AudioTime::ZERO, Vec2::default());
                let _ = step_baseline(
                    &mut baseline,
                    width,
                    height,
                    AudioTime::ZERO,
                    Vec2::default(),
                    0.0,
                );
                scene.transition = phase;
                let progress = smoothstep(phase);
                let shake = if translating {
                    Vec2::new(240.0, 120.0) * (height as f32 / 1080.0)
                } else {
                    Vec2::default()
                };
                let end = AudioTime::ZERO.shifted(1.0 / 60.0);
                let exchanged = step_scene(&mut scene, width, height, end, shake);
                let legacy = step_baseline(&mut baseline, width, height, end, shake, progress);
                ensure!(
                    exchanged.active_edges <= largest_edges,
                    "edge exchange exceeded larger model budget: {} > {}",
                    exchanged.active_edges,
                    largest_edges
                );
                ensure!(
                    legacy.active_edges == full_pair_edges,
                    "legacy baseline must submit both complete wireframes"
                );
                if !translating {
                    ensure!(
                        exchanged.lines.is_empty() && legacy.lines.is_empty(),
                        "two recorded poses must use the production swept path"
                    );
                    ensure!(
                        exchanged.ribbons.len() <= 2 * largest_edges,
                        "static transition must not exceed the larger model's triangle count"
                    );
                }
                for (policy, output) in [("edge_exchange", exchanged), ("global_alpha", legacy)] {
                    let timing = gpu.measure(output.frame(), samples)?;
                    let triangles = output.ribbons.len() + output.lines.len() * 2;
                    eprintln!("{width}x{height} {motion} phase {phase:.2} {policy}: {} edges / {triangles} triangles; GPU {:.3}/{:.3} ms median/p95",
                        output.active_edges,super::summary(&timing.gpu_ms)["median"].as_f64().unwrap(),super::summary(&timing.gpu_ms)["p95"].as_f64().unwrap());
                    gpu.write_preview(
                        output.frame(),
                        &directory.join(format!(
                            "{width}x{height}-{motion}-{:03}-{policy}.png",
                            (phase * 100.0).round() as u32
                        )),
                    )?;
                    cases.push(json!({
                        "width":width,"height":height,"motion":motion,"phase":phase,"eased_progress":progress,
                        "policy":policy,"active_edges":output.active_edges,"triangles":triangles,
                        "sharp_lines":output.lines.len(),"ribbons":output.ribbons.len(),
                        "timing":timing_json(&timing),
                    }));
                }
            }
        }
    }
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec_pretty(&json!({
            "models":ids,"model_edges":[ranking[0].1,ranking[1].1],"larger_model_edge_budget":largest_edges,
            "samples_per_case":samples,"adapters":adapters,"cases":cases,
            "method":"Production Scene::show/switch/update with frozen two-pose runtime history, fixed camera and no audio; still snapshots across whole transition, translation midpoint stress. Baseline uses fresh default-all ModelRuntime::update for both models, then global straight-alpha factors, preserving legacy geometry and GPU work. GPU timestamps exclude setup/geometry/UI/presentation.",
            "edge_counts":"active_edges counts selected logical edges before viewport culling; global alpha baseline submits both complete sets even when alpha0. Static ribbon triangle counts provide a separate emitted-geometry budget assertion.",
        }))?,
    )?;
    eprintln!(
        "Saved production transition measurements/previews to {}",
        directory.display()
    );
    Ok(())
}
