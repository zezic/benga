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
fn analyzer_visibility_keeps_the_presentation_background_retained() -> anyhow::Result<()> {
    let mut ui = Ui::new(&Config::default())?;
    let view = View::fit(Size::new(1920.0, 1080.0));
    let origin = Instant::now();
    assert!(draw_retained_frame(&mut ui, &view, origin, 0));
    assert!(!ui.analyzers_visible());
    ui.toggle_analyzers();
    assert!(ui.analyzers_visible());
    assert!(!draw_retained_frame(&mut ui, &view, origin, 1));
    ui.toggle_analyzers();
    assert!(!ui.analyzers_visible());
    assert!(!draw_retained_frame(&mut ui, &view, origin, 2));
    Ok(())
}

fn transitioning_presentation_rebuilds() -> anyhow::Result<(usize, usize)> {
    let mut ui = Ui::new(&Config::default())?;
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
