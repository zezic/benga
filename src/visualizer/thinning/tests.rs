use super::*;
use crate::visualizer::geometry::{Vec2, Vec3};

fn fixture(count: usize) -> Wireframe {
    let mut vertices = Vec::with_capacity(count * 2);
    let mut edges = Vec::with_capacity(count);
    let mut importance = Vec::with_capacity(count);
    for index in 0..count {
        let length = [0.005, 0.02, 0.08][index % 3];
        let a = vertices.len() as u16;
        vertices.push(Vec3::new(0.0, 0.0, 0.0));
        vertices.push(Vec3::new(length, 0.0, 0.0));
        edges.push([a, a + 1]);
        importance.push(if index % 2 == 0 { 2.5 } else { 1.0 });
    }
    Wireframe::from_edges(vertices, edges).with_edge_importance(importance)
}

fn geometry(count: usize, y: f32) -> Vec<LineGeometry> {
    (0..count)
        .map(|_| LineGeometry {
            start: Vec2::new(100.0, 100.0 + y),
            end: Vec2::new(200.0, 100.0 + y),
            start_depth_opacity: 1.0,
            end_depth_opacity: 1.0,
            visible: true,
        })
        .collect()
}

fn count_visible(thinning: &Thinning) -> usize {
    thinning
        .edges
        .iter()
        .filter(|edge| edge.alpha > 0.0)
        .count()
}

fn settle(
    thinning: &mut Thinning,
    frames: usize,
    fps: f32,
    normalized_speed: f32,
    strength: f32,
    focus: &[Option<u16>],
) {
    let count = thinning.edges.len();
    for frame in 0..=frames {
        let geometry = geometry(count, frame as f32 * normalized_speed * 1000.0 / fps);
        thinning.update(&geometry, 1.0 / fps, 1.0 / fps, 1000.0, strength, focus);
    }
}

#[test]
fn steady_and_disabled_models_keep_every_edge() {
    let wireframe = fixture(1200);
    let mut thinning = Thinning::new(&wireframe, 7);
    thinning.density = 1.0;
    let still = geometry(1200, 0.0);
    for _ in 0..20 {
        thinning.update(&still, 1.0 / 60.0, 1.0 / 60.0, 1000.0, 1.0, &[]);
    }
    assert_eq!(count_visible(&thinning), 1200);
    assert!(thinning.edges.iter().all(|edge| edge.alpha == 1.0));
    settle(&mut thinning, 20, 60.0, 8.0, 1.0, &[]);
    assert!(count_visible(&thinning) < 600);
    thinning.update(&still, 1.0 / 60.0, 1.0 / 60.0, 1000.0, 0.0, &[]);
    assert!(thinning.edges.iter().all(|edge| edge.alpha == 1.0));
}

#[test]
fn ordinary_motion_keeps_all_detail_and_faster_motion_thins_at_equal_elapsed_time() {
    let wireframe = fixture(1200);
    let mut at_60 = Thinning::new(&wireframe, 19);
    let mut at_120 = Thinning::new(&wireframe, 19);
    at_60.density = 1.0;
    at_120.density = 1.0;
    // This is about 15 px/frame at 1080p60: below the activation threshold.
    settle(&mut at_60, 15, 60.0, 0.93, 1.0, &[]);
    settle(&mut at_120, 30, 120.0, 0.93, 1.0, &[]);
    assert!(at_60
        .edges
        .iter()
        .chain(&at_120.edges)
        .all(|edge| edge.alpha == 1.0 && edge.pressure == 0.0 && !edge.moving));
    // Faster movement selects the same stable subset at either refresh rate.
    settle(&mut at_60, 15, 60.0, 2.0, 1.0, &[]);
    settle(&mut at_120, 30, 120.0, 2.0, 1.0, &[]);
    assert!(count_visible(&at_60) < 1000);
    assert_eq!(count_visible(&at_60), count_visible(&at_120));
}

#[test]
fn stable_weighted_bands_favor_features_and_keep_some_long_edges_thinable() {
    let wireframe = fixture(6000);
    let mut first = Thinning::new(&wireframe, 123);
    let mut second = Thinning::new(&wireframe, 123);
    first.density = 1.0;
    second.density = 1.0;
    settle(&mut first, 20, 60.0, 8.0, 1.0, &[]);
    settle(&mut second, 20, 60.0, 8.0, 1.0, &[]);
    assert_eq!(
        first
            .edges
            .iter()
            .map(|edge| edge.alpha)
            .collect::<Vec<_>>(),
        second
            .edges
            .iter()
            .map(|edge| edge.alpha)
            .collect::<Vec<_>>()
    );
    for band in 0..3 {
        let visible: Vec<_> = first
            .edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| edge.band == band && edge.alpha > 0.0)
            .collect();
        let strong = visible.iter().filter(|(index, _)| index % 2 == 0).count();
        let weak = visible.len() - strong;
        assert!(
            strong > weak,
            "band {band}: stronger features should survive more often"
        );
        assert!(visible.len() > 20 && visible.len() < 1200);
    }
    assert!(first
        .edges
        .iter()
        .any(|edge| edge.band == 2 && edge.alpha == 0.0));
}

#[test]
fn more_pressure_selects_nested_sets_and_restores_exactly() {
    let wireframe = fixture(1200);
    let mut thinning = Thinning::new(&wireframe, 81);
    thinning.density = 1.0;
    settle(&mut thinning, 20, 60.0, 8.0, 1.0, &[]);
    let normal: Vec<_> = thinning.edges.iter().map(|edge| edge.alpha).collect();
    settle(&mut thinning, 20, 60.0, 8.0, 2.0, &[]);
    for (edge, previous) in thinning.edges.iter().zip(normal) {
        assert!(edge.alpha <= previous);
    }
    let still = geometry(1200, 3000.0);
    // First pose can itself be a move; subsequent identical samples are still.
    for _ in 0..30 {
        thinning.update(&still, 1.0 / 60.0, 1.0 / 60.0, 1000.0, 2.0, &[]);
    }
    assert!(thinning.edges.iter().all(|edge| edge.alpha == 1.0));
}

#[test]
fn fades_reach_zero_and_are_independent_of_frame_rate_and_pixel_scale() {
    let wireframe = fixture(1200);
    let mut at_60 = Thinning::new(&wireframe, 72);
    let mut at_120 = Thinning::new(&wireframe, 72);
    let mut retina = Thinning::new(&wireframe, 72);
    at_60.density = 1.0;
    at_120.density = 1.0;
    retina.density = 1.0;
    settle(&mut at_60, 12, 60.0, 8.0, 1.0, &[]);
    settle(&mut at_120, 24, 120.0, 8.0, 1.0, &[]);
    for frame in 0..=12 {
        let mut geometry = geometry(1200, frame as f32 * 8.0 * 1000.0 / 60.0);
        for line in &mut geometry {
            line.start = line.start * 2.0;
            line.end = line.end * 2.0;
        }
        retina.update(&geometry, 1.0 / 60.0, 1.0 / 60.0, 2000.0, 1.0, &[]);
    }
    assert!(at_60.edges.iter().any(|edge| edge.alpha == 0.0));
    for ((a, b), scaled) in at_60.edges.iter().zip(&at_120.edges).zip(&retina.edges) {
        assert!((a.alpha - b.alpha).abs() < 1e-5);
        assert!((a.alpha - scaled.alpha).abs() < 1e-5);
    }
}

#[test]
fn stopping_motion_restores_geometry_in_sequence() {
    let wireframe = fixture(1200);
    let mut thinning = Thinning::new(&wireframe, 61);
    thinning.density = 1.0;
    settle(&mut thinning, 20, 60.0, 8.0, 1.0, &[]);
    let initial_count = count_visible(&thinning);
    let still: Vec<_> = thinning.edges.iter().map(|edge| edge.previous).collect();

    thinning.update(&still, 1.0 / 60.0, 1.0 / 60.0, 1000.0, 1.0, &[]);
    let first_quiet_count = count_visible(&thinning);
    assert!(first_quiet_count > initial_count);
    assert!(first_quiet_count < 1200);
    assert!(thinning.edges.iter().any(|edge| edge.alpha == 0.0));

    let mut previous = first_quiet_count;
    let mut growing_frames = 0;
    for _ in 1..30 {
        thinning.update(&still, 1.0 / 60.0, 1.0 / 60.0, 1000.0, 1.0, &[]);
        let count = count_visible(&thinning);
        assert!(count >= previous);
        growing_frames += usize::from(count > previous);
        previous = count;
    }
    assert!(growing_frames > 5);
    assert!(thinning
        .edges
        .iter()
        .all(|edge| edge.alpha == 1.0 && edge.pressure == 0.0));
}

#[test]
fn recovery_uses_elapsed_time_instead_of_frame_count() {
    let wireframe = fixture(1200);
    let mut at_60 = Thinning::new(&wireframe, 74);
    let mut at_120 = Thinning::new(&wireframe, 74);
    at_60.density = 1.0;
    at_120.density = 1.0;
    settle(&mut at_60, 12, 60.0, 8.0, 1.0, &[]);
    settle(&mut at_120, 24, 120.0, 8.0, 1.0, &[]);
    let still_60: Vec<_> = at_60.edges.iter().map(|edge| edge.previous).collect();
    let still_120: Vec<_> = at_120.edges.iter().map(|edge| edge.previous).collect();
    for _ in 0..6 {
        at_60.update(&still_60, 1.0 / 60.0, 1.0 / 60.0, 1000.0, 1.0, &[]);
    }
    for _ in 0..12 {
        at_120.update(&still_120, 1.0 / 120.0, 1.0 / 120.0, 1000.0, 1.0, &[]);
    }
    // At 100 ms, both rates have released the same rank cutoff. Discrete fade
    // sampling may differ by up to half a 60 Hz alpha-fade step near that cutoff.
    assert_eq!(count_visible(&at_60), count_visible(&at_120));
    assert!(count_visible(&at_60) < 1200);
    for (a, b) in at_60.edges.iter().zip(&at_120.edges) {
        assert!((a.pressure - b.pressure).abs() < 1e-5);
        assert!((a.alpha - b.alpha).abs() < 0.11);
    }
    for _ in 6..30 {
        at_60.update(&still_60, 1.0 / 60.0, 1.0 / 60.0, 1000.0, 1.0, &[]);
    }
    for _ in 12..60 {
        at_120.update(&still_120, 1.0 / 120.0, 1.0 / 120.0, 1000.0, 1.0, &[]);
    }
    assert!(at_60
        .edges
        .iter()
        .chain(&at_120.edges)
        .all(|edge| edge.alpha == 1.0 && edge.pressure == 0.0));
}

#[test]
fn selected_parts_receive_more_detail_without_forcing_every_edge_visible() {
    let wireframe = fixture(1200);
    let mut ordinary = Thinning::new(&wireframe, 42);
    let mut focused = Thinning::new(&wireframe, 42);
    ordinary.density = 1.0;
    focused.density = 1.0;
    settle(&mut ordinary, 20, 60.0, 8.0, 1.0, &[]);
    settle(&mut focused, 20, 60.0, 8.0, 1.0, &[Some(0)]);
    assert!(count_visible(&focused) > count_visible(&ordinary));
    assert!(count_visible(&focused) < 1200);
    for (a, b) in ordinary.edges.iter().zip(focused.edges) {
        assert!(b.alpha >= a.alpha);
    }
}

#[test]
fn each_mesh_part_retains_a_representative_stroke() {
    let count = 2400;
    let vertices = (0..count)
        .flat_map(|_| [Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.005, 0.0, 0.0)])
        .collect();
    let edges = (0..count)
        .map(|index| [(index * 2) as u16, (index * 2 + 1) as u16])
        .collect();
    let vertex_parts = (0..count)
        .flat_map(|index| [index as u16 % 12; 2])
        .collect();
    let edge_parts = (0..count).map(|index| index as u16 % 12).collect();
    let wireframe = Wireframe::from_parts(vertices, edges, vertex_parts, edge_parts, 12);
    let mut thinning = Thinning::new(&wireframe, 23);
    thinning.density = 1.0;
    settle(&mut thinning, 20, 60.0, 8.0, 2.0, &[]);
    assert!(count_visible(&thinning) < 100);
    for part in 0..12 {
        assert!(thinning
            .edges
            .iter()
            .any(|edge| edge.part == part && edge.alpha > 0.0));
    }
}

#[test]
fn small_models_and_temporal_resets_preserve_complete_detail() {
    let wireframe = fixture(1200);
    let mut thinning = Thinning::new(&wireframe, 9);
    settle(&mut thinning, 20, 60.0, 8.0, 2.0, &[]);
    assert!(thinning.edges.iter().all(|edge| edge.alpha == 1.0));
    thinning.density = 1.0;
    settle(&mut thinning, 20, 60.0, 8.0, 2.0, &[]);
    assert!(count_visible(&thinning) < 600);
    thinning.reset_motion();
    assert!(thinning
        .edges
        .iter()
        .all(|edge| edge.alpha == 1.0 && !edge.recorded));
}

#[test]
fn along_stroke_motion_does_not_remove_detail() {
    let wireframe = fixture(1200);
    let mut thinning = Thinning::new(&wireframe, 11);
    thinning.density = 1.0;
    for frame in 0..20 {
        let mut geometry = geometry(1200, 0.0);
        for line in &mut geometry {
            line.start.x += frame as f32 * 200.0;
            line.end.x += frame as f32 * 200.0;
        }
        thinning.update(&geometry, 1.0 / 60.0, 1.0 / 60.0, 1000.0, 2.0, &[]);
    }
    assert!(thinning.edges.iter().all(|edge| edge.alpha == 1.0));
}
