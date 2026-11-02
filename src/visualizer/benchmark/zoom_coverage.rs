//! CPU-only diagnostic of source-edge geometry in the zoom replay.
//! Area sums count overlapping continuous raster bounds repeatedly. They are
//! neither fragment counts nor a replacement for measured GPU execution time.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use anyhow::Context;
use serde_json::{json, Value};
use vello::{kurbo::Size, Scene};

use crate::{
    config::Config,
    frame_schedule::{Adjustment, FrameRateLimit, FrameTime},
    visualizer::{gpu_bridge::GpuModelFrame, Models, Ui, View},
    AudioEvent,
};

use super::zoom_workload;

type P = [f32; 2];
type V = [f32; 4];

fn xy(v: V) -> P {
    [v[0], v[1]]
}
fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}
fn dot(a: P, b: P) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}
fn length(a: P) -> f32 {
    dot(a, a).sqrt()
}
fn normalized(mut a: P) -> P {
    if dot(a, a) < 1e-10 {
        a = [1.0, 0.0];
    }
    let length = length(a);
    [a[0] / length, a[1] / length]
}

fn bounds(points: impl IntoIterator<Item = P>, padding: f32) -> [f32; 4] {
    let mut b = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for p in points {
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    }
    [
        b[0] - padding,
        b[1] - padding,
        b[2] + padding,
        b[3] + padding,
    ]
}

fn outside(b: [f32; 4], viewport: P) -> bool {
    b[2] < 0.0 || b[3] < 0.0 || b[0] > viewport[0] || b[1] > viewport[1]
}

fn clipped_aabb_area(b: [f32; 4], viewport: P) -> f64 {
    f64::from((b[2].min(viewport[0]) - b[0].max(0.0)).max(0.0))
        * f64::from((b[3].min(viewport[1]) - b[1].max(0.0)).max(0.0))
}

/// Clip an oriented rectangle against the physical viewport before measuring
/// its continuous area. This avoids counting arbitrarily far offscreen bounds.
fn clipped_oriented_area(local: [f32; 4], along: P, across: P, viewport: P) -> f64 {
    if local[2] <= local[0] || local[3] <= local[1] {
        return 0.0;
    }
    let corners = [
        [local[0], local[1]],
        [local[2], local[1]],
        [local[2], local[3]],
        [local[0], local[3]],
    ];
    let mut polygon: Vec<[f64; 2]> = corners
        .into_iter()
        .map(|p| {
            [
                f64::from(along[0] * p[0] + across[0] * p[1]),
                f64::from(along[1] * p[0] + across[1] * p[1]),
            ]
        })
        .collect();
    for (axis, limit, lower) in [
        (0, 0.0, true),
        (0, f64::from(viewport[0]), false),
        (1, 0.0, true),
        (1, f64::from(viewport[1]), false),
    ] {
        if polygon.is_empty() {
            return 0.0;
        }
        let old = std::mem::take(&mut polygon);
        let mut previous = *old.last().unwrap();
        let inside = |p: [f64; 2]| {
            if lower {
                p[axis] >= limit
            } else {
                p[axis] <= limit
            }
        };
        for current in old {
            let previous_inside = inside(previous);
            let current_inside = inside(current);
            if previous_inside != current_inside {
                let fraction = (limit - previous[axis]) / (current[axis] - previous[axis]);
                polygon.push([
                    previous[0] + (current[0] - previous[0]) * fraction,
                    previous[1] + (current[1] - previous[1]) * fraction,
                ]);
            }
            if current_inside {
                polygon.push(current);
            }
            previous = current;
        }
    }
    polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
        .map(|(a, b)| a[0] * b[1] - a[1] * b[0])
        .sum::<f64>()
        .abs()
        * 0.5
}

fn classify(frame: &GpuModelFrame, history: &[Vec<V>], viewport: P) -> Value {
    let width = frame.width.max(0.05);
    let half = (width * 0.5).max(0.5);
    let mut zero_ink = 0;
    let mut hidden = 0;
    let mut outside_edges = 0;
    let mut ordinary = 0;
    let mut parallel = 0;
    let mut parallel_intervals = 0;
    let mut ordinary_swept_aabb_area = 0.0;
    let mut parallel_swept_aabb_area = 0.0;
    let mut parallel_old_area = 0.0;
    let mut parallel_tight_area = 0.0;
    let mut total_old_unclipped = 0.0;
    let mut total_tight_unclipped = 0.0;
    for (index, edge) in frame.topology.edges.iter().enumerate() {
        if frame.styles[index][3] <= 0.0 {
            zero_ink += 1;
            continue;
        }
        let mut a = [
            history[0][edge[0] as usize],
            history[1][edge[0] as usize],
            history[2][edge[0] as usize],
        ];
        let mut b = [
            history[0][edge[1] as usize],
            history[1][edge[1] as usize],
            history[2][edge[1] as usize],
        ];
        let valid = [
            a[0][3].min(b[0][3]) >= 0.5,
            a[1][3].min(b[1][3]) >= 0.5,
            a[2][3].min(b[2][3]) >= 0.5,
        ];
        if !valid.into_iter().any(|v| v) {
            hidden += 1;
            continue;
        }
        // Exactly the production geometry shader's missing-pose substitution.
        if !valid[1] {
            let source = if valid[0] { 0 } else { 2 };
            a[1] = a[source];
            b[1] = b[source];
        }
        if !valid[0] {
            a[0] = a[1];
            b[0] = b[1];
        }
        if !valid[2] {
            a[2] = a[1];
            b[2] = b[1];
        }
        let swept = bounds(a.into_iter().chain(b).map(xy), half + 0.75);
        if outside(swept, viewport) {
            outside_edges += 1;
            continue;
        }
        let mut direction = sub(xy(b[1]), xy(a[1]));
        if dot(direction, direction) < 1e-10 {
            direction = sub(xy(b[0]), xy(a[0]));
        }
        let direction = normalized(direction);
        let normal = [-direction[1], direction[0]];
        let offsets = [
            dot(sub(xy(a[0]), xy(a[1])), normal),
            dot(sub(xy(b[0]), xy(b[1])), normal),
            dot(sub(xy(a[2]), xy(a[1])), normal),
            dot(sub(xy(b[2]), xy(b[1])), normal),
        ];
        let travel_a = length(sub(xy(a[1]), xy(a[0]))) + length(sub(xy(a[2]), xy(a[1])));
        let travel_b = length(sub(xy(b[1]), xy(b[0]))) + length(sub(xy(b[2]), xy(b[1])));
        let is_parallel = offsets.into_iter().map(f32::abs).fold(0.0, f32::max) <= half
            && travel_a.max(travel_b) > width;
        if !is_parallel {
            ordinary += 1;
            ordinary_swept_aabb_area += clipped_aabb_area(swept, viewport);
            continue;
        }
        parallel += 1;
        parallel_swept_aabb_area += clipped_aabb_area(swept, viewport);
        for interval in 0..2 {
            let points = [
                xy(a[interval]),
                xy(b[interval]),
                xy(a[interval + 1]),
                xy(b[interval + 1]),
            ];
            if outside(bounds(points, half + 0.75), viewport) {
                continue;
            }
            parallel_intervals += 1;
            let first = sub(points[1], points[0]);
            let last = sub(points[3], points[2]);
            let along = normalized([first[0] + last[0], first[1] + last[1]]);
            let across = [-along[1], along[0]];
            let padding = width.max(1.0) * 0.5 + 0.75;
            let old = bounds(points.map(|p| [dot(p, along), dot(p, across)]), padding);
            let mut tight = old;
            if old.into_iter().map(f32::abs).fold(0.0, f32::max) < 65536.0 {
                let center = dot(
                    [
                        (points[0][0] + points[2][0]) * 0.5,
                        (points[0][1] + points[2][1]) * 0.5,
                    ],
                    across,
                );
                let support = width * 0.5 + 0.5 + 0.125;
                tight[1] = tight[1].max(center - support);
                tight[3] = tight[3].min(center + support);
            }
            total_old_unclipped +=
                f64::from((old[2] - old[0]).max(0.0)) * f64::from((old[3] - old[1]).max(0.0));
            total_tight_unclipped += f64::from((tight[2] - tight[0]).max(0.0))
                * f64::from((tight[3] - tight[1]).max(0.0));
            parallel_old_area += clipped_oriented_area(old, along, across, viewport);
            parallel_tight_area += clipped_oriented_area(tight, along, across, viewport);
        }
    }
    json!({
        "source_edges":frame.topology.edges.len(),"width_pixels":width,
        "zero_ink_edges":zero_ink,"invisible_edges":hidden,"outside_edges":outside_edges,
        "ordinary_body_edges":ordinary,"parallel_edges":parallel,"parallel_intervals":parallel_intervals,
        "ordinary_source_swept_aabb_clipped_area_sum":ordinary_swept_aabb_area,
        "parallel_source_swept_aabb_clipped_area_sum":parallel_swept_aabb_area,
        "old_parallel_oriented_box_clipped_area_sum":parallel_old_area,
        "tight_parallel_oriented_box_clipped_area_sum":parallel_tight_area,
        "parallel_clipped_area_reduction_fraction":if parallel_old_area>0.0 {1.0-parallel_tight_area/parallel_old_area} else {0.0},
        "old_parallel_oriented_box_unclipped_area_sum":total_old_unclipped,
        "tight_parallel_oriented_box_unclipped_area_sum":total_tight_unclipped,
        "viewport_area":f64::from(viewport[0])*f64::from(viewport[1]),
        "scope":"CPU f32 mirror of visibility/parallel classification. Continuous clipped geometric bound areas sum overlapping intervals repeatedly; these are not fragment counts. Ordinary source swept AABBs are coarse context, not exact emitted-triangle areas. No GPU execution or readback.",
    })
}

fn select_heaviest(ui: &mut Ui) -> anyhow::Result<String> {
    let visualizer = ui.benchmark_visualizer_mut();
    visualizer.load_models();
    let Models::Ready(models) = &mut visualizer.models else {
        anyhow::bail!("models did not load");
    };
    let mut largest = (String::new(), 0);
    for _ in 0..models.position().1 {
        let model = models.current();
        let count = model.wireframe(visualizer.crease_threshold).edges().len();
        if count > largest.1 {
            largest = (model.id.clone(), count);
        }
        models.navigate(Adjustment::Increase);
    }
    for _ in 0..models.position().1 {
        if models.current().id == largest.0 {
            break;
        }
        models.navigate(Adjustment::Increase);
    }
    visualizer.scene.show(
        models.current_prepared(),
        visualizer.model_motion,
        0.5,
        &visualizer.style_tuning,
    );
    visualizer.sync_showcase();
    Ok(largest.0)
}

#[test]
#[ignore = "diagnostic source-coverage scan; CPU-only production projection replay"]
fn reports_zoom_source_coverage() -> anyhow::Result<()> {
    let mut config = Config::default();
    config.model_rotation = crate::visualizer::ModelRotation::Locked;
    let mut ui = Ui::new(&config)?;
    ui.dismiss_startup();
    ui.audio_event(AudioEvent::Config { samplerate: 48000 });
    let model = select_heaviest(&mut ui)?;
    let script = zoom_workload::ZoomWorkload::from_env()?;
    let viewport = [5120.0, 2880.0];
    let mut view = View::fit(Size::new(f64::from(viewport[0]), f64::from(viewport[1])));
    view.set_ui_scale(2.0);
    view.set_zoom(config.zoom);
    let origin = Instant::now() + Duration::from_secs(12);
    let mut scene = Scene::new();
    let mut history = VecDeque::new();
    let mut frames = Vec::new();
    for index in 0..40 {
        let start = origin + Duration::from_secs_f64(index as f64 / 60.0);
        let end = origin + Duration::from_secs_f64((index + 1) as f64 / 60.0);
        ui.audio_event(AudioEvent::Buffer {
            buf: vec![[0.1; 2]; 800],
            captured_at: start,
        });
        let inputs = script.drive(&mut ui, &mut view, (index + 1) as f64 / 60.0);
        scene.reset();
        let frame = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps60,
            FrameTime::new(start, end),
        );
        let projected = frame
            .overlay
            .gpu_models
            .first()
            .context("GPU projection bridge frame")?;
        assert!(projected.projection.is_some() && projected.projected.is_empty());
        history.push_back(projected.cpu_projected());
        while history.len() < 3 {
            history.push_back(history.back().expect("current projected pose").clone());
        }
        while history.len() > 3 {
            history.pop_front();
        }
        let mut coverage = classify(projected, history.make_contiguous(), viewport);
        coverage["animation_frame"] = json!(index);
        coverage["inputs"] = inputs;
        if [13, 32, 33].contains(&index) {
            eprintln!("ZOOM_COVERAGE {}", serde_json::to_string(&coverage)?);
        }
        frames.push(coverage);
    }
    let directory = std::env::var_os("BENGA_ZOOM_COVERAGE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("target/gpu-zoom-repro/coverage"));
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("results.json"),
        serde_json::to_vec_pretty(&json!({
            "model":model,"viewport":viewport,"script":script.metadata(),"frames":frames,
            "scope":"CPU-only source projection and geometric support accounting. No render, fragment count, timing claim or GPU readback.",
        }))?,
    )?;
    Ok(())
}
