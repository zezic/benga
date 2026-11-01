use super::*;
use crate::visualizer::geometry::{Vec3, Wireframe};
use std::time::Duration;

fn draw_retained_frame(ui: &mut Ui, view: &View, origin: Instant, index: u64) -> bool {
    let mut scene = Scene::new();
    let start = origin + Duration::from_secs_f64(index as f64 / 60.0);
    let end = origin + Duration::from_secs_f64((index + 1) as f64 / 60.0);
    let frame = ui.draw(
        &mut scene,
        view,
        FrameRateLimit::Fps60,
        FrameTime::new(start, end),
    );
    matches!(frame.background, Frame::Rebuilt(_))
}

#[test]
fn presentation_retains_background_when_hidden_line_readout_changes() -> anyhow::Result<()> {
    let mut ui = Ui::new(&Config::default())?;
    ui.set_presentation(true);
    let view = View::fit(Size::new(5120.0, 2880.0));
    let origin = Instant::now();
    assert!(draw_retained_frame(&mut ui, &view, origin, 0));
    assert!(!draw_retained_frame(&mut ui, &view, origin, 1));

    // Force the same cache-miss branch reached when a model transition changes
    // the selected line count. This readout is absent from the presentation face.
    ui.line_count = usize::MAX;
    assert!(
        !draw_retained_frame(&mut ui, &view, origin, 2),
        "a hidden line-count readout must not invalidate the retained presentation background"
    );
    Ok(())
}

#[test]
fn leaving_presentation_refreshes_hidden_readouts_before_painting_hud() -> anyhow::Result<()> {
    let mut ui = Ui::new(&Config::default())?;
    ui.set_presentation(true);
    let view = View::fit(Size::new(1280.0, 720.0));
    let origin = Instant::now();
    let _ = draw_retained_frame(&mut ui, &view, origin, 0);
    ui.line_count = usize::MAX;
    ui.scene_name = "stale hidden HUD".to_owned();
    ui.model_position = None;
    let _ = draw_retained_frame(&mut ui, &view, origin, 1);

    ui.set_presentation(false);
    assert!(draw_retained_frame(&mut ui, &view, origin, 2));
    assert_eq!(ui.line_count, ui.visualizer.line_count());
    assert_eq!(ui.scene_name, ui.visualizer.scene_name());
    assert_eq!(ui.model_position, ui.visualizer.model_position());
    Ok(())
}

#[test]
fn visible_hud_still_rebuilds_for_a_changed_line_readout() -> anyhow::Result<()> {
    let mut ui = Ui::new(&Config::default())?;
    ui.set_presentation(false);
    let view = View::fit(Size::new(1280.0, 720.0));
    let origin = Instant::now();
    let _ = draw_retained_frame(&mut ui, &view, origin, 0);
    ui.line_count = usize::MAX;
    assert!(draw_retained_frame(&mut ui, &view, origin, 1));
    assert_eq!(ui.line_count, ui.visualizer.line_count());
    Ok(())
}

#[test]
fn presentation_pauses_debug_work_and_exit_refreshes_the_first_visible_snapshot(
) -> anyhow::Result<()> {
    let mut ui = Ui::new(&Config::default())?;
    ui.set_presentation(false);
    ui.set_debug(true);
    let view = View::fit(Size::new(1280.0, 720.0));
    let origin = Instant::now();
    let _ = draw_retained_frame(&mut ui, &view, origin, 0);
    assert!(ui.debug_snapshot.is_some());

    ui.debug_refresh = DebugRefresh::Hz1;
    ui.debug_clock = 0.125;
    ui.set_presentation(true);
    let _ = draw_retained_frame(&mut ui, &view, origin, 1);
    assert_eq!(ui.debug_clock, 0.125);

    ui.set_presentation(false);
    assert!(ui.debug_snapshot.is_none());
    let _ = draw_retained_frame(&mut ui, &view, origin, 2);
    assert!(ui.debug_snapshot.is_some());
    assert_eq!(ui.debug_clock, 0.0);
    Ok(())
}

fn transitioning_presentation_rebuilds() -> anyhow::Result<(usize, usize)> {
    let mut ui = Ui::new(&Config::default())?;
    ui.set_presentation(true);
    ui.visualizer.load_models();
    // Identical long edges create a deterministic heavy model without relying
    // on which bundled model happens to be first in the shuffled catalog.
    let wireframe = Wireframe::from_edges(
        vec![Vec3::new(0.0, -0.2, 0.0), Vec3::new(0.0, 0.2, 0.0)],
        vec![[0, 1]; 24_000],
    );
    ui.visualizer
        .scene
        .current
        .as_mut()
        .expect("the bundled model loaded")
        .set_wireframe(wireframe);
    let mut view = View::fit(Size::new(5120.0, 2880.0));
    view.set_ui_scale(2.0);
    let origin = Instant::now();
    let _ = draw_retained_frame(&mut ui, &view, origin, 0);
    let _ = draw_retained_frame(&mut ui, &view, origin, 1);
    ui.visualizer.navigate_model(Adjustment::Increase);
    // Different deterministic edge counts ensure that the real edge-by-edge
    // transition changes the hidden readout over multiple frames.
    ui.visualizer
        .scene
        .current
        .as_mut()
        .expect("the entering model loaded")
        .set_wireframe(Wireframe::from_edges(
            vec![Vec3::new(0.0, -0.2, 0.0), Vec3::new(0.0, 0.2, 0.0)],
            vec![[0, 1]],
        ));
    let mut previous = ui.visualizer.line_count();
    let mut changed_counts = 0;
    let mut rebuilds = 0;
    for index in 2..42 {
        let rebuilt = draw_retained_frame(&mut ui, &view, origin, index);
        let count = ui.visualizer.line_count();
        changed_counts += usize::from(count != previous);
        rebuilds += usize::from(rebuilt);
        previous = count;
    }
    Ok((changed_counts, rebuilds))
}

#[test]
fn model_transitions_keep_the_presentation_face_retained() -> anyhow::Result<()> {
    let (changed_counts, rebuilds) = transitioning_presentation_rebuilds()?;
    assert!(
        changed_counts > 1,
        "the workload must exercise changing edge counts during a model transition"
    );
    assert_eq!(
        rebuilds, 0,
        "transitions must not turn hidden HUD updates into presentation background rasterizations"
    );
    Ok(())
}
