//! Deterministic fast dolly/magnification replay through production projection.
//! This drives camera inputs, never fabricated screen vertices or ribbon data.
use anyhow::{ensure, Context};
use serde_json::{json, Value};
use strum::EnumCount;

use crate::{
    config::Zoom,
    visualizer::{Property, PropertyGates, Smoother, Ui, View, BAND_COUNT},
};

pub(super) struct ZoomWorkload {
    near: f32,
    far: f32,
    zoom_min: f64,
    zoom_max: f64,
    hz: f64,
}

impl ZoomWorkload {
    pub fn from_env() -> anyhow::Result<Self> {
        fn number(name: &str, fallback: f64) -> anyhow::Result<f64> {
            match std::env::var(name) {
                Ok(value) => value.parse().with_context(|| format!("invalid {name}")),
                Err(std::env::VarError::NotPresent) => Ok(fallback),
                Err(error) => Err(error.into()),
            }
        }
        let near = number("BENGA_PIPELINE_ZOOM_NEAR", 0.7)?;
        let far = number("BENGA_PIPELINE_ZOOM_FAR", 4.2)?;
        let zoom_min = number("BENGA_PIPELINE_ZOOM_MIN", 0.7)?;
        let zoom_max = number("BENGA_PIPELINE_ZOOM_MAX", 3.0)?;
        let hz = number("BENGA_PIPELINE_ZOOM_HZ", 3.0)?;
        ensure!(
            near.is_finite() && (0.41..=12.0).contains(&near),
            "zoom near distance must be 0.41..=12"
        );
        ensure!(
            far.is_finite() && far >= near && far <= 12.0,
            "zoom far distance must be near..=12"
        );
        ensure!(
            zoom_min.is_finite()
                && zoom_max.is_finite()
                && zoom_min >= Zoom::MIN
                && zoom_max <= Zoom::MAX
                && zoom_min <= zoom_max,
            "camera zoom range must be {}..={}",
            Zoom::MIN,
            Zoom::MAX
        );
        ensure!(
            hz.is_finite() && (0.1..=15.0).contains(&hz),
            "zoom frequency must be 0.1..=15 Hz"
        );
        Ok(Self {
            near: near as f32,
            far: far as f32,
            zoom_min,
            zoom_max,
            hz,
        })
    }

    pub fn metadata(&self) -> Value {
        json!({
            "near_distance":self.near,"far_distance":self.far,
            "camera_zoom_min":self.zoom_min,"camera_zoom_max":self.zoom_max,"cycles_per_second":self.hz,
            "projection":"Production camera and near-plane visibility rules, fixed upright model orientation, sinusoidal log-space dolly plus camera magnification. All source edges are retained; the projection backend is recorded separately.",
        })
    }

    pub fn drive(&self, ui: &mut Ui, view: &mut View, seconds: f64) -> Value {
        let blend = 0.5 - 0.5 * (seconds * std::f64::consts::TAU * self.hz).cos();
        let distance =
            f64::from(self.far) * (f64::from(self.near) / f64::from(self.far)).powf(blend);
        let zoom = self.zoom_min * (self.zoom_max / self.zoom_min).powf(blend);
        view.set_zoom(
            Zoom::try_from(zoom.clamp(Zoom::MIN, Zoom::MAX)).expect("validated zoom range"),
        );
        let visualizer = ui.benchmark_visualizer_mut();
        visualizer.pending_actions.clear();
        let mut muted = [true; Property::COUNT];
        muted[Property::Line.index()] = false;
        visualizer.property_gates = PropertyGates {
            muted,
            soloed: [false; Property::COUNT],
        };
        visualizer.debug_music([0.8; BAND_COUNT], 0.8);
        if let Some(model) = &mut visualizer.scene.current {
            model.pose.distance = distance as f32;
            model.pose.yaw = 0.75;
            model.pose.pitch = 0.2;
            model.pitch = Smoother::new(0.2);
            model.pose.roll = 0.0;
            model.roll = Smoother::new(0.0);
        }
        json!({"time_seconds":seconds,"zoom_blend":blend,"pose_distance":distance,"camera_zoom":zoom})
    }
}

/// Diagnostic CPU scan outside every reported timing stage, identical in both
/// backends. Bounds are not tessellation-derived and retain offscreen geometry.
pub(super) fn snapshot(ui: &mut Ui, view: &View) -> Value {
    let visualizer = ui.benchmark_visualizer_mut();
    let Some(model) = &visualizer.scene.current else {
        return json!(null);
    };
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    let mut visible = 0;
    for vertex in (0..model.projected_vertex_count())
        .map(|index| model.projected_vertex(index))
        .filter(|v| v.visible && v.screen.x.is_finite() && v.screen.y.is_finite())
    {
        bounds[0] = bounds[0].min(vertex.screen.x);
        bounds[1] = bounds[1].min(vertex.screen.y);
        bounds[2] = bounds[2].max(vertex.screen.x);
        bounds[3] = bounds[3].max(vertex.screen.y);
        visible += 1;
    }
    json!({
        "pose_distance":model.pose.distance,"pose_scale":model.pose.scale,"pose_yaw":model.pose.yaw,
        "pose_pitch":model.pose.pitch,"camera_zoom":view.zoom_factor(),
        "visible_vertices":visible,"source_vertices":model.wireframe.vertex_count(),
        "projected_bounds":if visible>0 {Some(bounds)} else {None},
        "bounds_scan_scope":"Untimed CPU diagnostic reference projection using the current production camera; conservative source bounds, not GPU readback or summed raster coverage.",
    })
}

#[test]
fn gpu_zoom_snapshot_projects_lazy_source_vertices_for_diagnostics() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    use vello::{kurbo::Size, Scene};

    use crate::{
        config::{Config, ShutterFrames},
        frame_schedule::{FrameRateLimit, FrameTime},
    };

    let mut config = Config::default();
    config.shutter_frames = ShutterFrames::try_from(2.0)?;
    let mut ui = Ui::new(&config)?;
    ui.dismiss_startup();
    let visualizer = ui.benchmark_visualizer_mut();
    visualizer.load_models();
    visualizer.set_gpu_ribbons(true);
    visualizer.set_gpu_ribbons_supported(true);
    let mut view = View::fit(Size::new(1280.0, 720.0));
    let script = ZoomWorkload {
        near: 0.7,
        far: 4.2,
        zoom_min: 0.7,
        zoom_max: 3.0,
        hz: 3.0,
    };
    script.drive(&mut ui, &mut view, 1.0 / 60.0);
    let start = Instant::now();
    let frame = ui.draw(
        &mut Scene::new(),
        &view,
        FrameRateLimit::Fps60,
        FrameTime::new(start, start + Duration::from_secs_f64(1.0 / 60.0)),
    );
    assert!(frame.overlay.gpu_active);
    assert!(frame.overlay.gpu_models[0].projected.is_empty());
    let current = ui
        .benchmark_visualizer_mut()
        .scene
        .current
        .as_ref()
        .expect("bundled model is visible");
    assert!(current.vertices.is_empty());
    let captured = snapshot(&mut ui, &view);
    assert!(captured["visible_vertices"].as_u64().unwrap() > 0);
    assert!(captured["projected_bounds"]
        .as_array()
        .expect("lazy GPU projection still has diagnostic bounds")
        .iter()
        .all(|value| value.as_f64().unwrap().is_finite()));
    Ok(())
}
