//! Bounded scene-transition probes through the production Ui and offscreen renderer.
use super::*;
use crate::render::OffscreenRenderer;

fn configured_ui() -> anyhow::Result<(Ui, ThemeId)> {
    let mut config = Config::default();
    config.theme_preferences.auto_colors = false;
    config.model_rotation = super::super::ModelRotation::Locked;
    config.shutter_frames = ShutterFrames::try_from(2.0)?;
    config.theme = ThemeId::all().find(|theme| !theme.is_light()).unwrap();
    let next = ThemeId::all().find(|theme| theme.is_light()).unwrap();
    let mut ui = Ui::new(&config)?;
    ui.dismiss_startup();
    ui.visualizer.load_models();
    Ok((ui, next))
}

#[test]
fn color_interpolation_with_instant_model_swaps_avoids_background_rasterization(
) -> anyhow::Result<()> {
    let (mut ui, next) = configured_ui()?;
    let view = View::fit(Size::new(1920.0, 1080.0));
    let mut scene = Scene::new();
    let origin = Instant::now();
    let mut rebuilt = 0;
    let mut colors_changed = 0;
    let mut new_model_frames = 0;
    let original_topology =
        std::sync::Arc::clone(&ui.visualizer.scene.current.as_ref().unwrap().gpu_topology);
    let mut previous_color = ui.theme.colors.background;
    for index in 0..48 {
        let start = origin + Duration::from_secs_f64(index as f64 / 60.0);
        let end = origin + Duration::from_secs_f64((index + 1) as f64 / 60.0);
        if index == 24 {
            ui.navigate_model(Adjustment::Increase);
            ui.set_theme(next);
        }
        scene.reset();
        let frame = ui.draw_at_audio_time(
            &mut scene,
            &view,
            FrameTime::new(start, end),
            AudioInterval::new(
                crate::audio::timeline::AudioTime::ZERO.shifted(index as f64 / 60.0),
                crate::audio::timeline::AudioTime::ZERO.shifted((index + 1) as f64 / 60.0),
            ),
        );
        let frame_color = match frame.background {
            Frame::Solid(color) => Some(color),
            _ => None,
        };
        if index >= 24 {
            rebuilt += usize::from(matches!(frame.background, Frame::Rebuilt(_)));
            assert_eq!(frame.overlay.gpu_models.len(), 1);
            new_model_frames += usize::from(!std::sync::Arc::ptr_eq(
                &frame.overlay.gpu_models[0].topology,
                &original_topology,
            ));
        }
        let color = ui.theme.colors.background;
        assert_eq!(
            frame_color,
            Some(color),
            "the actual clear color must follow the interpolated theme"
        );
        colors_changed += usize::from(color != previous_color);
        previous_color = color;
    }
    assert!(
        colors_changed > 1,
        "the theme must visibly interpolate over multiple frames"
    );
    assert_eq!(
        new_model_frames, 24,
        "the complete new model must replace the old model on the first frame"
    );
    assert_eq!(
        rebuilt, 0,
        "smooth color changes must avoid full presentation background rasterization"
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU; bounded timing diagnostic, including output readback"]
fn measures_scene_transition_stages() -> anyhow::Result<()> {
    let (width, height) = std::env::var("BENGA_TRANSITION_EXTENT")
        .ok()
        .and_then(|value| {
            let (width, height) = value.split_once('x')?;
            Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?))
        })
        .unwrap_or((1920, 1080));
    let view = View::fit(Size::new(f64::from(width), f64::from(height)));
    let mut renderer = OffscreenRenderer::new(width, height)?;
    let with_audio = std::env::var("BENGA_TRANSITION_AUDIO").as_deref() == Ok("1");
    let vector_background =
        std::env::var("BENGA_TRANSITION_VECTOR_BACKGROUND").as_deref() == Ok("1");
    let mut cases = Vec::new();
    for (name, colors, model) in [
        ("settled", false, false),
        ("colors", true, false),
        ("model", false, true),
        ("both", true, true),
    ] {
        let (mut ui, next) = configured_ui()?;
        ui.validate_gpu_models(renderer.gpu_ribbon_edge_capacity())?;
        ui.set_plate_mask(Some(renderer.plate_mask_image()));
        let mut scene = Scene::new();
        let mut vector_backdrop = Scene::new();
        let mut previous_background = None;
        let origin = Instant::now();
        let mut draw_ms = Vec::new();
        let mut render_ms = Vec::new();
        let mut background_rebuilds = 0;
        let mut single_model_frames = 0;
        let mut new_model_frames = 0;
        let original_topology =
            std::sync::Arc::clone(&ui.visualizer.scene.current.as_ref().unwrap().gpu_topology);
        let mut frames = Vec::new();
        for index in 0..48 {
            let start = origin + Duration::from_secs_f64(index as f64 / 60.0);
            let end = origin + Duration::from_secs_f64((index + 1) as f64 / 60.0);
            if with_audio {
                let samples: Vec<_> = (index * 800..(index + 1) * 800)
                    .map(|sample| {
                        let t = sample as f32 / 48_000.0;
                        let gain = 0.08 + 0.15 * (t * 3.0).sin().abs();
                        let value = gain
                            * ((std::f32::consts::TAU * 63.0 * t).sin()
                                + 0.5 * (std::f32::consts::TAU * 320.0 * t).sin()
                                + 0.3 * (std::f32::consts::TAU * 1800.0 * t).sin());
                        [value; 2]
                    })
                    .collect();
                ui.consume_buf(&samples, start);
                // Preserve analyzer-driven motion while keeping the scheduled
                // model/theme exchange identical in every comparison.
                ui.visualizer.pending_actions.clear();
                ui.visualizer.deferred_snares.clear();
            }
            let before = Instant::now();
            if index == 24 {
                if model {
                    ui.navigate_model(Adjustment::Increase);
                }
                if colors {
                    ui.set_theme(next);
                }
            }
            scene.reset();
            let mut frame = ui.draw_at_audio_time(
                &mut scene,
                &view,
                FrameTime::new(start, end),
                AudioInterval::new(
                    crate::audio::timeline::AudioTime::ZERO.shifted(index as f64 / 60.0),
                    crate::audio::timeline::AudioTime::ZERO.shifted((index + 1) as f64 / 60.0),
                ),
            );
            if vector_background {
                let Frame::Solid(color) = frame.background else {
                    anyhow::bail!("the matched vector control requires a solid Ui backdrop");
                };
                let rebuilt = previous_background != Some(color);
                if rebuilt {
                    vector_backdrop.reset();
                    vector_backdrop.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        color,
                        None,
                        &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
                    );
                    previous_background = Some(color);
                }
                frame.background = if rebuilt {
                    Frame::Rebuilt(&vector_backdrop)
                } else {
                    Frame::Retained(&vector_backdrop)
                };
            }
            let draw = before.elapsed().as_secs_f64() * 1_000.0;
            let rebuilt = matches!(frame.background, Frame::Rebuilt(_));
            let count = frame.overlay.gpu_models.len();
            assert_eq!(count, 1, "instant swaps must submit one complete model");
            let new_model =
                !std::sync::Arc::ptr_eq(&frame.overlay.gpu_models[0].topology, &original_topology);
            let source_edges: usize = frame
                .overlay
                .gpu_models
                .iter()
                .map(|model| model.topology.edges.len())
                .sum();
            let style_bytes: usize = frame
                .overlay
                .gpu_models
                .iter()
                .map(|model| std::mem::size_of_val(model.styles.as_slice()))
                .sum();
            let before = Instant::now();
            std::hint::black_box(renderer.render(&scene, frame)?);
            let render = before.elapsed().as_secs_f64() * 1_000.0;
            if index >= 24 {
                draw_ms.push(draw);
                render_ms.push(render);
                background_rebuilds += usize::from(rebuilt);
                single_model_frames += usize::from(count == 1);
                new_model_frames += usize::from(new_model);
                frames.push(serde_json::json!({
                    "frame": index - 24,
                    "draw_ms": draw,
                    "render_and_readback_ms": render,
                    "background_rebuilt": rebuilt,
                    "gpu_models": count,
                    "new_model": new_model,
                    "source_edges": source_edges,
                    "edge_style_bytes": style_bytes,
                }));
            }
        }
        for (phase, range) in [("transition", 0..12), ("after-color", 12..24)] {
            let mut phase_draw = draw_ms[range.clone()].to_vec();
            let mut phase_render = render_ms[range].to_vec();
            phase_draw.sort_by(f64::total_cmp);
            phase_render.sort_by(f64::total_cmp);
            eprintln!(
                "scene-transition {name} {phase}: draw median {:.3} ms / peak {:.3} ms; render+readback median {:.3} ms / peak {:.3} ms",
                phase_draw[6], phase_draw[11], phase_render[6], phase_render[11],
            );
        }
        draw_ms.sort_by(f64::total_cmp);
        render_ms.sort_by(f64::total_cmp);
        eprintln!(
            "scene-transition {name} {width}x{height}: draw median {:.3} ms / peak {:.3} ms; render+readback median {:.3} ms / peak {:.3} ms; background rebuilds {background_rebuilds}/24; single-model frames {single_model_frames}/24; new-model frames {new_model_frames}/24",
            draw_ms[12], draw_ms[23], render_ms[12], render_ms[23],
        );
        eprintln!("scene-transition {name} first frame: {}", frames[0]);
        cases.push(serde_json::json!({
            "case": name,
            "background_rebuilds": background_rebuilds,
            "single_model_frames": single_model_frames,
            "new_model_frames": new_model_frames,
            "draw_median_ms": draw_ms[12],
            "draw_peak_ms": draw_ms[23],
            "render_and_readback_median_ms": render_ms[12],
            "render_and_readback_peak_ms": render_ms[23],
            "frames": frames,
        }));
        if model {
            assert_eq!(
                new_model_frames, 24,
                "the probe must switch to the new model on its first measured frame"
            );
        }
        if colors {
            if let Ok(limit) = std::env::var("BENGA_TRANSITION_MAX_BACKGROUND_REBUILDS") {
                let limit: usize = limit.parse()?;
                assert!(background_rebuilds <= limit, "color interpolation rebuilt the presentation background {background_rebuilds} times (limit {limit})");
            }
        }
    }
    let output = std::env::var_os("BENGA_TRANSITION_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/scene-transition-profile.json")
        });
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &output,
        serde_json::to_vec_pretty(&serde_json::json!({
            "scope": "Production Ui draw and offscreen render with synchronous RGBA8 output readback. Timings include readback and are not native presentation cadence or isolated GPU execution costs.",
            "width": width,
            "height": height,
            "audio": with_audio,
            "vector_background": vector_background,
            "instant_model_swap": true,
            "warmup_frames": 24,
            "measured_frames": 24,
            "color_transition_frames": 12,
            "cases": cases,
        }))?,
    )?;
    eprintln!("scene-transition results: {}", output.display());
    Ok(())
}
