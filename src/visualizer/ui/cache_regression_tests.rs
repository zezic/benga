use super::*;
use crate::visualizer::geometry::{Vec3, Wireframe};
use std::time::Duration;

fn draw_solid_frame(ui: &mut Ui, view: &View, origin: Instant, index: u64) -> Color {
    let mut scene = Scene::new();
    let start = origin + Duration::from_secs_f64(index as f64 / 60.0);
    let end = origin + Duration::from_secs_f64((index + 1) as f64 / 60.0);
    let frame = ui.draw(
        &mut scene,
        view,
        FrameRateLimit::Fps60,
        FrameTime::new(start, end),
    );
    let Frame::Solid(color) = frame.background else {
        panic!("the presentation background must use a solid GPU clear");
    };
    color
}

#[test]
fn analyzer_visibility_keeps_the_presentation_background_retained() -> anyhow::Result<()> {
    let mut ui = Ui::new(&Config::default())?;
    let view = View::fit(Size::new(1920.0, 1080.0));
    let origin = Instant::now();
    let background = draw_solid_frame(&mut ui, &view, origin, 0);
    assert!(!ui.analyzers_visible());
    ui.toggle_analyzers();
    assert!(ui.analyzers_visible());
    assert_eq!(draw_solid_frame(&mut ui, &view, origin, 1), background);
    ui.toggle_analyzers();
    assert!(!ui.analyzers_visible());
    assert_eq!(draw_solid_frame(&mut ui, &view, origin, 2), background);
    Ok(())
}

fn instant_presentation_swaps() -> anyhow::Result<()> {
    let mut config = Config::default();
    config.theme_preferences.auto_colors = false;
    let mut ui = Ui::new(&config)?;
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
    let background = draw_solid_frame(&mut ui, &view, origin, 0);
    assert_eq!(draw_solid_frame(&mut ui, &view, origin, 1), background);
    assert_eq!(ui.visualizer.line_count(), 24_000);
    ui.visualizer.navigate_model(Adjustment::Increase);
    // A new single-edge model must replace the complete old model at once.
    ui.visualizer
        .scene
        .current
        .as_mut()
        .expect("the entering model loaded")
        .set_wireframe(Wireframe::from_edges(
            vec![Vec3::new(0.0, -0.2, 0.0), Vec3::new(0.0, 0.2, 0.0)],
            vec![[0, 1]],
        ));
    assert!(ui.visualizer.scene.outgoing.is_none());
    assert_eq!(ui.visualizer.line_count(), 1);
    for index in 2..42 {
        assert_eq!(draw_solid_frame(&mut ui, &view, origin, index), background);
        assert_eq!(ui.visualizer.line_count(), 1);
        assert!(ui.visualizer.scene.outgoing.is_none());
    }
    Ok(())
}

#[test]
fn instant_model_swaps_keep_the_solid_background_and_replace_all_edges() -> anyhow::Result<()> {
    instant_presentation_swaps()
}
