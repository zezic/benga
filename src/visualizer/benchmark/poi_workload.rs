//! Repeatable real POI entry, switch, orbit and FOV actions for pipeline probes.
//! The camera is advanced by the application's showcase springs, never by
//! fabricated projected geometry or directly assigned camera coordinates.

use std::sync::OnceLock;

use anyhow::{ensure, Context};
use serde_json::{json, Value};

use super::super::{Action, Adjustment, ModelRotation, Models, Rng, Ui, BAND_COUNT};

const SEED: u64 = 0x504f_495f_464c_4947;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Static,
    Flight,
}

pub(super) fn configure(ui: &mut Ui, mode: Mode) -> anyhow::Result<Value> {
    let visualizer = ui.benchmark_visualizer_mut();
    if !matches!(visualizer.models, Models::Ready(_)) {
        visualizer.load_models();
    }
    let catalog = visualizer
        .catalog
        .as_ref()
        .context("POI benchmark needs the catalog")?;
    let Models::Ready(models) = &mut visualizer.models else {
        anyhow::bail!("POI benchmark models did not load")
    };
    let mut ranking = Vec::new();
    for _ in 0..models.position().1 {
        let model = models.current();
        let resolved = catalog.weapons.get(&model.id).map_or(0, |weapon| {
            weapon
                .poi
                .iter()
                .filter(|poi| model.anchor(&poi.anchor.name).is_some())
                .count()
        });
        if resolved >= 2 {
            ranking.push((
                model.id.clone(),
                model.wireframe(visualizer.crease_threshold).edges().len(),
                resolved,
            ));
        }
        models.navigate(Adjustment::Increase);
    }
    ranking.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let (id, raw_edges, resolved_pois) = ranking
        .first()
        .context("no model has at least two resolved POIs")?
        .clone();
    for _ in 0..models.position().1 {
        if models.current().id == id {
            break;
        }
        models.navigate(Adjustment::Increase);
    }
    ensure!(models.current().id == id, "selected POI model disappeared");
    visualizer.scene.show(
        models.current_prepared(),
        visualizer.model_motion,
        0.5,
        &visualizer.style_tuning,
    );
    visualizer.sync_showcase();
    visualizer.model_rotation = ModelRotation::Locked;
    visualizer.rng = Rng::new(SEED);
    let original_settings = visualizer.showcase.settings().clone();
    if mode == Mode::Static {
        let mut settings = original_settings.clone();
        settings.orbit_speed = 0.0;
        settings.dolly_depth = 0.0;
        visualizer.showcase.set_settings(settings);
    }
    visualizer.pending_actions.clear();
    // A locked-model drop enters focus through the same path as the app,
    // including upright-flip handling and its entry gate.
    visualizer.push_action(Action::Drop(0.0));
    ensure!(
        visualizer.showcase_poi_count() >= 2,
        "POI selection lost resolved anchors"
    );
    Ok(json!({
        "model_id": id, "raw_edges": raw_edges, "resolved_pois": resolved_pois,
        "mode": if mode == Mode::Static {"poi_static"} else {"poi_flight"},
        "seed": SEED, "showcase_settings": original_settings,
        "static_overrides": if mode == Mode::Static {json!({"orbit_speed":0.0,"dolly_depth":0.0})} else {Value::Null},
        "actions":"Enter via locked-model Drop. At the first measured frame request Snare+Kick; then kick every quarter second and switch once per second. Actual entry/cooldown gates remain in force.",
    }))
}

fn measured_start() -> usize {
    static START: OnceLock<usize> = OnceLock::new();
    *START.get_or_init(|| {
        let gpu_warmup = std::env::var("BENGA_PIPELINE_WARMUP")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120);
        let cpu_warmup = std::env::var("BENGA_PIPELINE_CPU_WARMUP")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        cpu_warmup + gpu_warmup
    })
}

/// Runs before Ui::draw. Returned events document requests; snapshots after
/// drawing show whether production entry/cooldown gates accepted each change.
pub(super) fn drive(ui: &mut Ui, index: usize, fps: u32, mode: Mode) -> Value {
    let visualizer = ui.benchmark_visualizer_mut();
    visualizer.pending_actions.clear();
    visualizer.debug_music([0.8; BAND_COUNT], 0.8);
    let mut events = Vec::new();
    if visualizer.showcase_mode_name() == "NORMAL" {
        visualizer.push_action(Action::Drop(0.0));
        events.push("enter_focus");
    }
    if mode == Mode::Flight && index >= measured_start() {
        let elapsed = index - measured_start();
        let tick = elapsed as u64 * 4 / u64::from(fps);
        let previous_tick = elapsed.saturating_sub(1) as u64 * 4 / u64::from(fps);
        if elapsed == 0 || tick != previous_tick {
            if tick % 4 == 0 {
                visualizer.snare_hits += 1;
                visualizer.push_action(Action::Snare);
                events.push("snare");
            }
            visualizer.kick_hits += 1;
            visualizer.push_action(Action::Kick(1.0));
            events.push("kick");
        }
    }
    json!({"frame":index,"time_seconds":index as f64/f64::from(fps),"events":events})
}

pub(super) fn snapshot(ui: &mut Ui) -> Value {
    let visualizer = ui.benchmark_visualizer_mut();
    let frame = visualizer.showcase.frame();
    json!({
        "mode":visualizer.showcase_mode_name(),
        "poi_id":visualizer.showcase_target_id(),
        "poi_index":visualizer.showcase_target_index(),
        "poi_switches":visualizer.showcase_poi_switches(),
        "focus_blend":frame.focus.blend,
        "fov_degrees":frame.focus.fov.to_degrees(),
        "focus_center":[frame.focus.center.x,frame.focus.center.y,frame.focus.center.z],
        "focus_radius":frame.focus.radius,
        "orbit_yaw_degrees":frame.focus.orbit_yaw.to_degrees(),
        "orbit_pitch_degrees":frame.focus.orbit_pitch.to_degrees(),
        "dolly_scale":frame.focus.dolly_scale,
        "highlight_part":frame.highlight_part,
        "model_motion":frame.focus.model_motion,
        "active_edges":visualizer.line_count(),
    })
}
