use super::*;
use benga::theme::{Accent, Theme, ThemeId};

fn colors(theme: ThemeId) -> (ThemeColors, Color) {
    (Theme::new(theme).colors, theme.color(Accent::Foreground))
}

#[test]
fn first_start_help_can_be_dismissed_and_update_preferences_persist() -> anyhow::Result<()> {
    let mut config = Config::default();
    assert!(config.updates.check_on_start);
    assert!(!config.updates.auto_install);
    let mut desktop = Desktop::new(&config, None, true)?;
    desktop.open = true;
    let mut scene = Scene::new();
    for size in [Size::new(1280.0, 720.0), Size::new(480.0, 320.0)] {
        let (colors, ink) = colors(config.theme);
        desktop.draw(&mut scene, size, 1.0, &config, colors, ink);
        assert!(desktop
            .targets
            .iter()
            .all(|(r, _)| r.x0 >= 0.0 && r.y0 >= 0.0 && r.x1 <= size.width && r.y1 <= size.height));
    }
    let point = desktop
        .targets
        .iter()
        .find(|(_, a)| matches!(a, Action::ToggleChecks))
        .unwrap()
        .0
        .center();
    assert!(!desktop.click(point, &mut config));
    assert!(!config.updates.check_on_start);
    desktop.close(&mut config);
    let restored: Config = ron::from_str(&ron::to_string(&config)?)?;
    assert!(restored.onboarding_seen);
    assert!(!restored.updates.check_on_start);
    assert!(!Desktop::new(&restored, None, true)?.open);
    Ok(())
}

#[test]
fn every_enabled_button_and_update_badge_respond_to_hover_in_both_tones() -> anyhow::Result<()> {
    let config = Config::default();
    for theme in [
        ThemeId::default(),
        ThemeId::all().find(|theme| theme.is_light()).unwrap(),
    ] {
        let (colors, ink) = colors(theme);
        let mut desktop = Desktop::new(&config, None, true)?;
        desktop.available = Some(Release {
            version: "1.2.3".into(),
            artifact: crate::updater::Artifact {
                url: String::new(),
                sha256: String::new(),
                size: 1,
            },
        });
        for open in [true, false] {
            desktop.open = open;
            desktop.set_pointer(None);
            let mut scene = Scene::new();
            desktop.draw(
                &mut scene,
                Size::new(1280.0, 720.0),
                1.0,
                &config,
                colors,
                ink,
            );
            let base = scene.encoding().draw_data.clone();
            let targets = desktop.targets.clone();
            assert!(!targets.is_empty());
            for (target, _) in targets {
                assert!(desktop.contains(target.center()));
                desktop.set_pointer(Some(target.center()));
                scene.reset();
                desktop.draw(
                    &mut scene,
                    Size::new(1280.0, 720.0),
                    1.0,
                    &config,
                    colors,
                    ink,
                );
                assert_ne!(
                    scene.encoding().draw_data,
                    base,
                    "hover must visibly change the button"
                );
            }
            desktop.set_pointer(None);
            scene.reset();
            desktop.draw(
                &mut scene,
                Size::new(1280.0, 720.0),
                1.0,
                &config,
                colors,
                ink,
            );
            assert_eq!(
                scene.encoding().draw_data,
                base,
                "leaving the dialog restores the normal state"
            );
        }
    }
    Ok(())
}

#[test]
fn theme_changes_recolor_the_complete_dialog() -> anyhow::Result<()> {
    let config = Config::default();
    let mut desktop = Desktop::new(&config, None, true)?;
    desktop.open = true;
    let mut scene = Scene::new();
    let (dark, dark_ink) = colors(ThemeId::default());
    desktop.draw(
        &mut scene,
        Size::new(1280.0, 720.0),
        1.0,
        &config,
        dark,
        dark_ink,
    );
    let dark_draw = scene.encoding().draw_data.clone();
    scene.reset();
    let (light, light_ink) = colors(ThemeId::all().find(|theme| theme.is_light()).unwrap());
    desktop.draw(
        &mut scene,
        Size::new(1280.0, 720.0),
        1.0,
        &config,
        light,
        light_ink,
    );
    assert_ne!(scene.encoding().draw_data, dark_draw);
    Ok(())
}

#[test]
#[ignore = "requires a compute-capable GPU; writes dark/light dialog and hover previews"]
fn renders_first_start_help() -> anyhow::Result<()> {
    use std::{fs::File, io::BufWriter};
    use vello::{util::RenderContext, wgpu, RenderParams};
    let width = 1200_u32;
    let height = 750_u32;
    let config = Config::default();
    let mut context = RenderContext::new();
    let id = pollster::block_on(benga::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    let mut renderer = benga::render::renderer(&device.device)?;
    let texture = device.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Help dialog preview"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output = std::env::temp_dir().join("benga-dialog-previews");
    std::fs::create_dir_all(&output)?;
    for light in [false, true] {
        let theme = ThemeId::all()
            .find(|theme| {
                if light {
                    theme.is_light()
                } else {
                    theme.name() == "Benga"
                }
            })
            .unwrap();
        let (colors, ink) = colors(theme);
        let mut desktop = Desktop::new(&config, None, true)?;
        desktop.open = true;
        for hovered in [false, true] {
            let mut scene = Scene::new();
            desktop.draw(
                &mut scene,
                Size::new(width as f64, height as f64),
                1.0,
                &config,
                colors,
                ink,
            );
            if hovered {
                desktop.set_pointer(Some(
                    desktop
                        .targets
                        .iter()
                        .find(|(_, action)| *action == Action::ToggleChecks)
                        .unwrap()
                        .0
                        .center(),
                ));
                scene.reset();
                desktop.draw(
                    &mut scene,
                    Size::new(width as f64, height as f64),
                    1.0,
                    &config,
                    colors,
                    ink,
                );
            }
            renderer.render_to_texture(
                &device.device,
                &device.queue,
                &scene,
                &texture.create_view(&Default::default()),
                &RenderParams {
                    base_color: colors.background,
                    width,
                    height,
                    antialiasing_method: benga::render::ANTIALIASING,
                },
            )?;
            let stride = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let buffer = device.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Help readback"),
                size: u64::from(stride * height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),
                        rows_per_image: Some(height),
                    },
                },
                texture.size(),
            );
            device.queue.submit([encoder.finish()]);
            let (tx, rx) = mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = tx.send(result);
                });
            device.device.poll(wgpu::PollType::wait_indefinitely())?;
            rx.recv()??;
            let mapped = buffer.slice(..).get_mapped_range()?;
            let pixels: Vec<u8> = mapped
                .chunks(stride as usize)
                .flat_map(|row| row[..width as usize * 4].iter().copied())
                .collect();
            let filename = format!(
                "dialog-{}{}.png",
                if light { "light" } else { "dark" },
                if hovered { "-hover" } else { "" }
            );
            let mut encoder = png::Encoder::new(
                BufWriter::new(File::create(output.join(filename))?),
                width,
                height,
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&pixels)?;
        }
    }
    eprintln!("Dialog previews: {}", output.display());
    Ok(())
}
