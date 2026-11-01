//! Optional GPU snapshots of the production graph layouts over a real scene.

use super::{
    tests::{presentation_pixels, write_preview},
    *,
};
use crate::visualizer::{
    presentation::{FormationPreview, Stage},
    ArrangementSetting, ModelMotion, ModelRotation,
};

fn preview_config() -> Config {
    let mut config = Config::default();
    config.theme_preferences.auto_colors = false;
    config.model_rotation = ModelRotation::Locked;
    config.model_motion = ModelMotion::Still;
    for setting in [
        ArrangementSetting::MixLossDb,
        ArrangementSetting::ShapeDistance,
        ArrangementSetting::TrendChange,
        ArrangementSetting::ChangedFraction,
        ArrangementSetting::GapHold,
    ] {
        config.detectors.arrangement.set(setting, setting.range().1);
    }
    config.detectors.arrangement.gap_rms_db = -80.0;
    config
}

fn percussion_audio(frame: usize, noise_state: &mut u32) -> Vec<[f32; 2]> {
    (frame * 800..(frame + 1) * 800)
        .map(|sample| {
            let time = sample as f32 / 48_000.0;
            *noise_state = noise_state
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            let noise = (*noise_state >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            let tau = std::f32::consts::TAU;
            let kick = 0.8 * (-time.rem_euclid(0.5) * 44.0).exp() * (tau * 62.0 * time).sin();
            let snare = 0.5 * (-(time - 0.25).rem_euclid(1.0) * 55.0).exp() * noise;
            let hat = 0.2 * (-time.rem_euclid(0.125) * 95.0).exp() * (tau * 7_200.0 * time).sin();
            let pad = 0.15 * (tau * 220.0 * time).sin();
            [pad + kick + snare + hat, pad + kick - snare + hat]
        })
        .collect()
}

#[test]
#[ignore = "requires a compute-capable GPU"]
fn renders_musical_graph_compositions() -> anyhow::Result<()> {
    let Some(directory) = std::env::var_os("BENGA_GRAPH_PREVIEW_DIR") else {
        return Ok(());
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory)?;
    let selected = std::env::var("BENGA_GRAPH_PREVIEW_CASE").ok();
    for (name, width, height, seed, light) in [
        ("dark", 1920, 1080, 1, false),
        ("group", 1920, 1080, 17, false),
        ("light", 1920, 1080, 37, true),
        ("compact", 1280, 720, 17, false),
        ("tall", 1024, 768, 37, false),
        ("ultrawide", 3440, 1080, 71, false),
    ] {
        if selected.as_ref().is_some_and(|selected| selected != name) {
            continue;
        }
        let mut ui = Ui::new(&preview_config())?;
        ui.dismiss_startup();
        if light {
            ui.set_theme(ron::from_str("\"Tomorrow\"")?);
        }
        ui.visualizer
            .set_parameter(Parameter::SelectorEntryKicks, 32.0);
        ui.visualizer.set_parameter(Parameter::SnareFlipEvery, 32.0);
        ui.audio_event(AudioEvent::Config { samplerate: 48_000 });
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let stage = Stage::new(view.viewport);
        let origin = Instant::now();
        let mut noise_state = 0x8193_9ab1_u32;
        let pixels = presentation_pixels(&mut ui, &view, 360, false, None, |frame, ui| {
            let input = percussion_audio(frame, &mut noise_state);
            ui.consume_buf(
                &input,
                origin + std::time::Duration::from_secs_f64(frame as f64 / 60.0),
            );
            if frame == 300 {
                assert_eq!(
                    ui.presentation
                        .preview_graphs(&ui.visualizer, &mut ui.fonts, &stage, seed),
                    3,
                    "the production layout must fit the preview heatmap group for {name}"
                );
            }
        })?;
        write_preview(
            &directory.join(format!("graphs-{name}.png")),
            width,
            height,
            &pixels,
        )?;
    }
    Ok(())
}

#[test]
#[ignore = "requires a compute-capable GPU"]
fn renders_mixed_graph_formations() -> anyhow::Result<()> {
    let Some(directory) = std::env::var_os("BENGA_FORMATION_PREVIEW_DIR") else {
        return Ok(());
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory)?;
    let selected = std::env::var("BENGA_GRAPH_PREVIEW_CASE").ok();
    for (name, width, height, preview, light) in [
        ("mixed", 1920, 1080, FormationPreview::Mixed, false),
        ("sidecar", 1920, 1080, FormationPreview::Sidecar, false),
        ("stepped", 1920, 1080, FormationPreview::Stepped, false),
        ("split", 1920, 1080, FormationPreview::Split, false),
        ("compact", 1280, 720, FormationPreview::Sidecar, false),
        ("light", 1920, 1080, FormationPreview::Split, true),
    ] {
        if selected.as_ref().is_some_and(|selected| selected != name) {
            continue;
        }
        let mut ui = Ui::new(&preview_config())?;
        ui.dismiss_startup();
        if light {
            ui.set_theme(ron::from_str("\"Tomorrow\"")?);
        }
        ui.visualizer
            .set_parameter(Parameter::SelectorEntryKicks, 32.0);
        ui.visualizer.set_parameter(Parameter::SnareFlipEvery, 32.0);
        ui.audio_event(AudioEvent::Config { samplerate: 48_000 });
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let stage = Stage::new(view.viewport);
        let origin = Instant::now();
        let mut noise_state = 0x8193_9ab1_u32;
        let pixels = presentation_pixels(&mut ui, &view, 360, false, None, |frame, ui| {
            ui.consume_buf(
                &percussion_audio(frame, &mut noise_state),
                origin + std::time::Duration::from_secs_f64(frame as f64 / 60.0),
            );
            if frame == 300 {
                let count = ui.presentation.preview_formation(
                    &ui.visualizer,
                    &mut ui.fonts,
                    &stage,
                    19,
                    preview,
                );
                assert!(count >= 2, "the {name} formation must fit the viewport");
                if matches!(preview, FormationPreview::Split) {
                    assert_eq!(count, 4, "paired formations keep all four panels");
                }
            }
        })?;
        write_preview(
            &directory.join(format!("formation-{name}.png")),
            width,
            height,
            &pixels,
        )?;
    }
    Ok(())
}

#[test]
#[ignore = "requires a compute-capable GPU"]
fn renders_vertex_following_poi_graphs() -> anyhow::Result<()> {
    use crate::visualizer::Rng;

    let Some(directory) = std::env::var_os("BENGA_POI_GRAPH_PREVIEW_DIR") else {
        return Ok(());
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory)?;
    let selected = std::env::var("BENGA_GRAPH_PREVIEW_CASE").ok();
    for (name, width, height, light, defocus_at) in [
        ("dark", 1920, 1080, false, None),
        ("compact", 1280, 720, false, None),
        ("light", 1920, 1080, true, None),
        ("coasting", 1920, 1080, false, Some(465)),
        ("graph-tail", 1920, 1080, false, Some(435)),
    ] {
        if selected.as_ref().is_some_and(|selected| selected != name) {
            continue;
        }
        let mut ui = Ui::new(&preview_config())?;
        ui.dismiss_startup();
        ui.visualizer.load_models();
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        assert_eq!(ui.visualizer.model_id(), Some("rif_ak47"));
        ui.visualizer
            .set_parameter(Parameter::SelectorEntryKicks, 32.0);
        ui.visualizer.set_parameter(Parameter::SnareFlipEvery, 32.0);
        ui.visualizer
            .set_parameter(Parameter::SnarePoiSwitches, 32.0);
        if light {
            ui.set_theme(ron::from_str("\"Tomorrow\"")?);
        }
        ui.audio_event(AudioEvent::Config { samplerate: 48_000 });
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let stage = Stage::new(view.viewport);
        let origin = Instant::now();
        let mut noise_state = 0x8193_9ab1_u32;
        let pixels = presentation_pixels(&mut ui, &view, 480, false, None, |frame, ui| {
            // Warm real graph data, then hold the chosen POI while its camera
            // and vertex callouts keep moving through their normal springs.
            if frame < 300 {
                ui.consume_buf(
                    &percussion_audio(frame, &mut noise_state),
                    origin + std::time::Duration::from_secs_f64(frame as f64 / 60.0),
                );
            }
            if frame == 300 {
                ui.visualizer.showcase.enter_focus(&mut Rng::new(7), 1.1);
            }
            if frame == 390 {
                assert!(
                    ui.presentation
                        .preview_poi_graphs(&ui.visualizer, &mut ui.fonts, &stage, 71)
                        > 0,
                    "vertex graphs must fit alongside the POI cards for {name}"
                );
            }
            if defocus_at == Some(frame) {
                ui.visualizer.showcase.exit_focus();
            }
        })?;
        assert_eq!(
            ui.visualizer.showcase_mode(),
            if defocus_at.is_some() {
                crate::visualizer::showcase::ShowcaseMode::Normal
            } else {
                crate::visualizer::showcase::ShowcaseMode::Poi
            }
        );
        assert!(
            ui.presentation.poi_graph_count() > 0,
            "POI graphs must spawn for {name}"
        );
        write_preview(
            &directory.join(format!("poi-graphs-{name}.png")),
            width,
            height,
            &pixels,
        )?;
    }
    Ok(())
}
