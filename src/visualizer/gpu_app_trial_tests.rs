//! Ignored, bounded visual smoke through the real Ui and offscreen composition.
use std::{
    fs::File,
    io::BufWriter,
    path::Path,
    time::{Duration, Instant},
};

use super::*;
use crate::{
    config::{Config, Zoom},
    frame_schedule::FrameTime,
    render::OffscreenRenderer,
    AudioEvent,
};
use vello::{kurbo::Size, Scene as VectorScene};

fn capture(path: &Path, width: u32, height: u32, pixels: &[u8]) -> anyhow::Result<()> {
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(pixels)?;
    Ok(())
}

fn select_slide_model(visualizer: &mut Visualizer) {
    visualizer.load_models();
    let total = visualizer.model_position().expect("bundled models load").1;
    for _ in 0..total {
        let current = visualizer.scene.current.as_ref().unwrap();
        let port = visualizer
            .showcase
            .pois()
            .iter()
            .find(|poi| poi.ejection_port)
            .cloned();
        if let Some(port) = port.filter(|_| current.slide_vertices.iter().any(|slide| *slide)) {
            let parts = match &visualizer.models {
                Models::Ready(models) => models.current().part_count(),
                _ => unreachable!("bundled models were loaded above"),
            };
            // Keep one real catalog anchor, making the bounded slide/focus trial
            // independent of which POI the production shuffle picks first.
            visualizer.showcase.set_model(vec![port], parts);
            visualizer.scene.recycle_outgoing();
            visualizer.set_diagnosis_suppressed(false);
            visualizer.diagnosis.next_in = 0.0;
            visualizer.showcase.settings_mut().selector_entry_kicks = 1;
            visualizer.showcase.settings_mut().selector_exit_triggers = 1;
            return;
        }
        visualizer.navigate_model(Adjustment::Increase);
    }
    panic!("the trial needs a bundled ejection-port POI with moving slide vertices");
}

#[test]
#[ignore = "requires a GPU; writes bounded production-Ui trial captures"]
fn production_ui_gpu_trial_with_audio_focus_transitions_resize_and_switches() -> anyhow::Result<()>
{
    let mut config = Config::default();
    config.shutter_frames = ShutterFrames::try_from(4.0)?;
    config.model_motion = ModelMotion::Swept;
    config.model_rotation = ModelRotation::Locked;
    config.theme_preferences.auto_colors = false;
    let dark = ThemeId::all()
        .find(|theme| !theme.is_light() && !config.theme_preferences.is_disliked(*theme))
        .unwrap();
    let light = ThemeId::all()
        .find(|theme| theme.is_light() && !config.theme_preferences.is_disliked(*theme))
        .unwrap();
    config.theme = dark;
    let mut ui = Ui::new(&config)?;
    ui.dismiss_startup();
    ui.audio_event(AudioEvent::Config { samplerate: 48_000 });
    select_slide_model(ui.benchmark_visualizer_mut());

    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/gpu-ribbon-integration/trial");
    std::fs::create_dir_all(&output)?;
    let (mut width, mut height) = (640, 360);
    let mut renderer = OffscreenRenderer::new(width, height)?;
    ui.validate_gpu_models(renderer.gpu_ribbon_edge_capacity())?;
    ui.set_plate_mask(Some(renderer.plate_mask_image()));
    let mut view = View::fit(Size::new(f64::from(width), f64::from(height)));
    let mut scene = VectorScene::new();
    let origin = Instant::now();
    let mut seen_selector = false;
    let mut seen_focus = false;
    let mut seen_slide = false;
    let mut seen_instant_swap = false;
    let mut last_topology = None;
    let mut seen_diagnosis = false;
    let mut seen_gpu_blur_off = false;
    let mut last_blur_off_generation = None;
    let mut seen_additive = false;
    let mut seen_multiply = false;
    let mut first_projection = None;
    let mut projection_changed = false;
    for index in 0..80 {
        let start_seconds = f64::from(index) / 30.0;
        let end_seconds = f64::from(index + 1) / 30.0;
        let start = origin + Duration::from_secs_f64(start_seconds);
        let end = origin + Duration::from_secs_f64(end_seconds);
        let audio: Vec<_> = (0..1600)
            .map(|sample| {
                let t = (f64::from(index * 1600 + sample) / 48_000.0) as f32;
                let gain = 0.08 + 0.15 * (t * 3.0).sin().abs();
                let value = gain
                    * ((TAU * 63.0 * t).sin()
                        + 0.5 * (TAU * 320.0 * t).sin()
                        + 0.3 * (TAU * 1800.0 * t).sin());
                [value; 2]
            })
            .collect();
        ui.consume_buf(&audio, start);
        {
            let visualizer = ui.benchmark_visualizer_mut();
            // Preserve real analyzer-driven motion while scheduling discrete
            // production callbacks independently of detector scheduling jitter.
            visualizer.pending_actions.clear();
            visualizer.deferred_snares.clear();
            let action = match index {
                3 => Some(Action::Kick(0.8)),
                4 => Some(Action::Hat),
                7 => Some(Action::Snare),
                10 | 36 => Some(Action::Drop(0.8)),
                _ => None,
            };
            if let Some(action) = action {
                visualizer.handle_action(action, 0.5, 0.75);
                seen_selector |= visualizer.showcase.selector_active();
            }
        }
        match index {
            20 => ui.set_theme(light),
            28 => {
                width = 800;
                height = 450;
                renderer = OffscreenRenderer::new(width, height)?;
                ui.validate_gpu_models(renderer.gpu_ribbon_edge_capacity())?;
                ui.set_plate_mask(Some(renderer.plate_mask_image()));
                view = View::fit(Size::new(f64::from(width), f64::from(height)));
            }
            40 => ui.navigate_model(Adjustment::Increase),
            43 => ui.set_model_motion(ModelMotion::Still),
            46 => ui.set_model_motion(ModelMotion::Swept),
            50 => ui.navigate_model(Adjustment::Decrease),
            53 => ui.navigate_model(Adjustment::Increase),
            57 => ui.set_theme(dark),
            60 => view.set_zoom(Zoom::try_from(1.4)?),
            64 => ui.set_model_motion(ModelMotion::Still),
            68 => ui.set_model_motion(ModelMotion::Swept),
            _ => {}
        }
        let expected_topology = Arc::clone(
            &ui.benchmark_visualizer_mut()
                .scene
                .current
                .as_ref()
                .expect("the trial has a selected model")
                .gpu_topology,
        );
        scene.reset();
        let frame = ui.draw_at_audio_time(
            &mut scene,
            &view,
            FrameTime::new(start, end),
            AudioInterval::new(
                AudioTime::ZERO.shifted(start_seconds),
                AudioTime::ZERO.shifted(end_seconds),
            ),
        );
        seen_additive |= frame.overlay.model_composite == ModelComposite::Additive;
        seen_multiply |= frame.overlay.model_composite == ModelComposite::Multiplicative;
        seen_diagnosis |= !frame.overlay.lines.is_empty();
        {
            assert_eq!(
                frame.overlay.gpu_models.len(),
                1,
                "an instant model swap submits only the current model"
            );
            let topology = &frame.overlay.gpu_models[0].topology;
            assert!(Arc::ptr_eq(topology, &expected_topology));
            if let Some(previous) = &last_topology {
                seen_instant_swap |= !Arc::ptr_eq(previous, topology);
            }
            last_topology = Some(Arc::clone(topology));
            if (64..68).contains(&index) {
                seen_gpu_blur_off = true;
                let generation = frame.overlay.gpu_models[0].reset;
                if let Some(previous) = last_blur_off_generation {
                    assert_ne!(
                        generation, previous,
                        "blur-off frames reseed their GPU poses"
                    );
                }
                last_blur_off_generation = Some(generation);
            }
            let projection = frame.overlay.gpu_models[0].cpu_projected();
            assert!(!projection.is_empty());
            if let Some(first) = &first_projection {
                projection_changed |= first != &projection;
            } else {
                first_projection = Some(projection.clone());
            }
        }
        let pixels = renderer.render(&scene, frame)?;
        assert_eq!(pixels.len(), (width * height * 4) as usize);
        assert!(
            pixels
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != pixels[..3]),
            "trial frame {index} must contain ink above its background"
        );
        let visualizer = ui.benchmark_visualizer_mut();
        assert!(visualizer.scene.outgoing.is_none());
        let current = visualizer.scene.current.as_ref().unwrap();
        assert!((0..current.active_edges).all(|edge| current.edge_dissolve.alpha(edge) == 1.0));
        let showcase = visualizer.showcase.frame();
        seen_focus |= showcase.focus.blend > 0.05;
        seen_slide |= showcase.slide_offset.length() > 1e-5;
        if [9, 19, 27, 35, 42, 45, 49, 52, 56, 67, 79].contains(&index) {
            let name = format!("frame-{index:02}-gpu.png");
            capture(&output.join(name), width, height, &pixels)?;
        }
    }
    assert!(
        seen_selector && seen_focus && seen_slide,
        "selector, focus and slide must all be exercised"
    );
    assert!(
        seen_instant_swap,
        "the trial must replace the model immediately during playback"
    );
    assert!(
        seen_diagnosis,
        "diagnostic strokes must survive the model composition"
    );
    assert!(
        seen_gpu_blur_off,
        "blur-off model rendering must stay on the GPU"
    );
    assert!(
        seen_additive && seen_multiply,
        "both production theme composites must run"
    );
    assert!(
        projection_changed,
        "music/camera motion must update the uploaded shared vertices"
    );
    Ok(())
}
