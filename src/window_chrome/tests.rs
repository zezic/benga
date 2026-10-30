use super::*;
use crate::theme::{Accent, Theme, ThemeId};

#[test]
fn platform_buttons_keep_native_order_and_ignore_scene_zoom() -> anyhow::Result<()> {
    for (kind, expected) in [
        (
            Kind::Windows,
            [
                WindowButton::Minimize,
                WindowButton::Maximize,
                WindowButton::Close,
            ],
        ),
        (
            Kind::Linux,
            [
                WindowButton::Close,
                WindowButton::Maximize,
                WindowButton::Minimize,
            ],
        ),
    ] {
        let chrome = Chrome::with_kind(kind)?;
        for scale in [1.0, 1.5, 2.0] {
            for ((button, rect), expected) in chrome.buttons(640.0).zip(expected) {
                assert_eq!(button, expected);
                let center = rect.center();
                assert_eq!(
                    chrome.hit_test(
                        Point::new(center.x * scale, center.y * scale),
                        Size::new(640.0 * scale, 360.0 * scale),
                        scale,
                        false,
                        false
                    ),
                    Some(Hit::Button(button))
                );
            }
        }
    }
    Ok(())
}

#[test]
fn every_edge_and_corner_has_a_resize_cursor_until_maximized() -> anyhow::Result<()> {
    let chrome = Chrome::with_kind(Kind::Windows)?;
    let size = Size::new(640.0, 360.0);
    for (point, edge, cursor) in [
        ((1.0, 180.0), ResizeDirection::West, CursorIcon::EwResize),
        ((639.0, 180.0), ResizeDirection::East, CursorIcon::EwResize),
        ((320.0, 1.0), ResizeDirection::North, CursorIcon::NsResize),
        ((320.0, 359.0), ResizeDirection::South, CursorIcon::NsResize),
        (
            (1.0, 1.0),
            ResizeDirection::NorthWest,
            CursorIcon::NwseResize,
        ),
        (
            (639.0, 1.0),
            ResizeDirection::NorthEast,
            CursorIcon::NeswResize,
        ),
        (
            (1.0, 359.0),
            ResizeDirection::SouthWest,
            CursorIcon::NeswResize,
        ),
        (
            (639.0, 359.0),
            ResizeDirection::SouthEast,
            CursorIcon::NwseResize,
        ),
    ] {
        let hit = chrome.hit_test(point.into(), size, 1.0, false, false);
        assert_eq!(hit, Some(Hit::Resize(edge)));
        assert_eq!(hit.unwrap().cursor(), cursor);
        assert!(!matches!(
            chrome.hit_test(point.into(), size, 1.0, false, true),
            Some(Hit::Resize(_))
        ));
    }
    assert_eq!(
        chrome.hit_test(Point::new(-1.0, 12.0), size, 1.0, false, false),
        None
    );
    Ok(())
}

#[test]
fn native_metrics_reserve_traffic_lights_and_fullscreen_releases_the_scene() -> anyhow::Result<()> {
    let mut chrome = Chrome::with_kind(Kind::MacOS)?;
    chrome.height = 48.0;
    chrome.left_inset = 104.0;
    let size = Size::new(1280.0, 720.0);
    assert_eq!(
        chrome.hit_test(Point::new(200.0, 50.0), size, 2.0, false, false),
        None
    );
    assert_eq!(
        chrome.hit_test(Point::new(220.0, 94.0), size, 2.0, false, false),
        Some(Hit::Drag)
    );
    assert_eq!(
        chrome.hit_test(Point::new(220.0, 98.0), size, 2.0, false, false),
        None
    );
    chrome.pointer(Some(Point::new(20.0, 20.0)));
    assert!(chrome.is_hovered(size, 2.0, false));
    assert!(!chrome.is_hovered(size, 2.0, true));
    for kind in [Kind::MacOS, Kind::Windows, Kind::Linux] {
        let chrome = Chrome::with_kind(kind)?;
        for point in [
            Point::new(1.0, 1.0),
            Point::new(500.0, 14.0),
            Point::new(1279.0, 719.0),
        ] {
            assert_eq!(chrome.hit_test(point, size, 1.0, true, false), None);
        }
    }
    Ok(())
}

#[test]
fn titlebar_double_click_is_local_and_can_be_reset_between_contexts() -> anyhow::Result<()> {
    let mut chrome = Chrome::with_kind(Kind::Linux)?;
    let now = Instant::now();
    assert!(!chrome.drag_double_click(Point::new(200.0, 14.0), now));
    assert!(chrome.drag_double_click(Point::new(202.0, 14.0), now + Duration::from_millis(200)));
    assert!(!chrome.drag_double_click(Point::new(200.0, 14.0), now + Duration::from_millis(300)));
    chrome.reset_drag_clicks();
    assert!(!chrome.drag_double_click(Point::new(200.0, 14.0), now + Duration::from_millis(400)));
    assert!(!chrome.drag_double_click(Point::new(210.0, 14.0), now + Duration::from_millis(500)));
    assert!(!chrome.drag_double_click(Point::new(210.0, 14.0), now + Duration::from_millis(1000)));
    Ok(())
}

fn palette() -> (ThemeColors, Color) {
    let id = ThemeId::all()
        .find(|theme| theme.name() == "Benga")
        .unwrap();
    (Theme::new(id).colors, id.color(Accent::Foreground))
}

#[test]
fn logo_is_centered_on_the_window_without_colliding_with_controls() -> anyhow::Result<()> {
    for kind in [Kind::MacOS, Kind::Windows, Kind::Linux] {
        let mut chrome = Chrome::with_kind(kind)?;
        chrome.height = 32.0;
        chrome.left_inset = 96.0;
        for width in [220.0, 360.0, 640.0, 1280.0] {
            if let Some(area) = chrome.logo_area(width) {
                assert!((area.center().x - width * 0.5).abs() < 1e-9);
                assert!(area.y0 >= 5.0 && area.y1 <= chrome.height - 5.0);
                let (left, right) = match kind {
                    Kind::MacOS => (chrome.left_inset + 12.0, width - 14.0),
                    Kind::Linux => (NATIVE_INSET + 10.0, width - 14.0),
                    Kind::Windows => (14.0, width - 3.0 * WINDOWS_CELL - 14.0),
                };
                assert!(area.x0 >= left && area.x1 <= right);
            }
        }
    }
    Ok(())
}

#[test]
fn logo_fades_independently_of_the_window_controls() -> anyhow::Result<()> {
    let (colors, ink) = palette();
    for kind in [Kind::MacOS, Kind::Windows, Kind::Linux] {
        let mut chrome = Chrome::with_kind(kind)?;
        let mut encodings = Vec::new();
        for opacity in [0.0, 0.5, 1.0] {
            chrome.set_logo_opacity(opacity);
            let mut scene = Scene::new();
            assert!(chrome.draw(
                &mut scene,
                Size::new(640.0, 360.0),
                1.0,
                colors,
                ink,
                1.0,
                false,
                false,
                true
            ));
            encodings.push(scene.encoding().draw_data.clone());
        }
        assert_ne!(encodings[0], encodings[1]);
        assert_ne!(encodings[1], encodings[2]);
        assert_ne!(encodings[0], encodings[2]);
    }
    Ok(())
}

#[test]
fn hover_pressed_and_opacity_are_visible_without_hiding_nonmac_controls() -> anyhow::Result<()> {
    let (colors, ink) = palette();
    for kind in [Kind::Windows, Kind::Linux] {
        let mut chrome = Chrome::with_kind(kind)?;
        let mut scene = Scene::new();
        assert!(chrome.draw(
            &mut scene,
            Size::new(640.0, 360.0),
            1.0,
            colors,
            ink,
            0.0,
            false,
            false,
            true
        ));
        let normal = scene.encoding().draw_data.clone();
        let (_, rect) = chrome
            .buttons(640.0)
            .find(|(b, _)| *b == WindowButton::Close)
            .unwrap();
        chrome.pointer(Some(rect.center()));
        scene.reset();
        chrome.draw(
            &mut scene,
            Size::new(640.0, 360.0),
            1.0,
            colors,
            ink,
            0.0,
            false,
            false,
            true,
        );
        let hovered = scene.encoding().draw_data.clone();
        assert_ne!(normal, hovered);
        chrome.set_pressed(Some(WindowButton::Close));
        scene.reset();
        chrome.draw(
            &mut scene,
            Size::new(640.0, 360.0),
            1.0,
            colors,
            ink,
            0.0,
            false,
            false,
            true,
        );
        assert_ne!(hovered, scene.encoding().draw_data);
        scene.reset();
        assert!(!chrome.draw(
            &mut scene,
            Size::new(640.0, 360.0),
            1.0,
            colors,
            ink,
            1.0,
            true,
            false,
            true
        ));
    }
    let mut chrome = Chrome::with_kind(Kind::MacOS)?;
    let mut scene = Scene::new();
    assert!(!chrome.draw(
        &mut scene,
        Size::new(640.0, 360.0),
        1.0,
        colors,
        ink,
        0.0,
        false,
        false,
        true
    ));
    assert!(chrome.draw(
        &mut scene,
        Size::new(640.0, 360.0),
        1.0,
        colors,
        ink,
        0.5,
        false,
        false,
        true
    ));
    Ok(())
}

#[test]
#[ignore = "requires a compute-capable GPU; writes platform chrome previews"]
fn renders_platform_chrome() -> anyhow::Result<()> {
    use std::{fs::File, io::BufWriter};
    use vello::{util::RenderContext, wgpu, RenderParams};
    let output = std::path::PathBuf::from("target/chrome-previews");
    std::fs::create_dir_all(&output)?;
    let width = 960_u32;
    let height = 240_u32;
    let mut context = RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    let mut renderer = crate::render::renderer(&device.device)?;
    let texture = device.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Window chrome preview"),
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
    let stride = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    for (kind, name) in [
        (Kind::MacOS, "macos"),
        (Kind::Windows, "windows"),
        (Kind::Linux, "linux"),
    ] {
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
            let colors = Theme::new(theme).colors;
            let ink = theme.color(Accent::Foreground);
            let mut chrome = Chrome::with_kind(kind)?;
            if kind == Kind::MacOS {
                chrome.height = 44.0;
                chrome.left_inset = 100.0;
            }
            let mut scene = Scene::new();
            let size = Size::new(f64::from(width), f64::from(height));
            for x in (-240..960).step_by(80) {
                scene.stroke(
                    &Stroke::new(1.0),
                    Affine::IDENTITY,
                    ink.with_alpha(0.24),
                    None,
                    &Line::new((f64::from(x), 0.0), (f64::from(x + 240), 240.0)),
                );
            }
            if kind != Kind::MacOS {
                let (_, rect) = chrome
                    .buttons(size.width)
                    .find(|(button, _)| *button == WindowButton::Close)
                    .unwrap();
                chrome.pointer(Some(rect.center()));
            }
            chrome.draw(&mut scene, size, 1.0, colors, ink, 1.0, false, false, true);
            renderer.render_to_texture(
                &device.device,
                &device.queue,
                &scene,
                &texture.create_view(&Default::default()),
                &RenderParams {
                    base_color: colors.background,
                    width,
                    height,
                    antialiasing_method: crate::render::ANTIALIASING,
                },
            )?;
            let buffer = device.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Chrome readback"),
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
            let (tx, rx) = std::sync::mpsc::channel();
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
            let mut encoder = png::Encoder::new(
                BufWriter::new(File::create(output.join(format!(
                    "{name}-{}.png",
                    if light { "light" } else { "dark" }
                )))?),
                width,
                height,
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&pixels)?;
            drop(mapped);
            buffer.unmap();
        }
    }
    Ok(())
}
