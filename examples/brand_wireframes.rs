//! Render brand artwork with Benga's actual model projection and GPU line renderer.
//! `cargo run --release --example brand_wireframes -- [output-directory]`
use std::{
    fs::File,
    io::BufWriter,
    path::{Path, PathBuf},
    sync::mpsc,
};

use anyhow::{ensure, Context};
use benga::{
    audio::timeline::{AudioInterval, AudioTime},
    config::ShutterFrames,
    frame_schedule::Adjustment,
    theme::ThemeId,
    visualizer::{
        Frame, LineWidth, MixPoint, ModelComposite, ModelMotion, ModelRotation, Palette, Parameter,
        Renderer, SelectableInput, StyleInput, Visualizer,
    },
};
use vello::{kurbo::Size, util::RenderContext, wgpu};

const WIDTH: u32 = 2000;
const HEIGHT: u32 = 1400;

fn main() -> anyhow::Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/brand-wireframes"));
    std::fs::create_dir_all(&output)?;
    let theme = ThemeId::all()
        .find(|theme| theme.name() == "Benga")
        .context("Benga theme is bundled")?;
    let palette = Palette::from_theme(theme);
    let mut context = RenderContext::new();
    let id = pollster::block_on(benga::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let extent = wgpu::Extent3d {
        width: WIDTH,
        height: HEIGHT,
        depth_or_array_layers: 1,
    };
    let background = device.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Transparent brand background"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    device.queue.write_texture(
        background.as_image_copy(),
        &vec![0; (WIDTH * HEIGHT * 4) as usize],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(WIDTH * 4),
            rows_per_image: Some(HEIGHT),
        },
        extent,
    );
    let target = device.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Brand wireframe render"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let background_view = background.create_view(&Default::default());
    let target_view = target.create_view(&Default::default());
    let mut renderer = Renderer::new(&device.device, format);
    let mut visualizer = Visualizer::new();
    visualizer.set_model_rotation(ModelRotation::Locked);
    visualizer.set_model_motion(ModelMotion::Still);
    visualizer.set_model_composite(ModelComposite::Additive);
    visualizer.set_diagnosis_suppressed(true);
    visualizer.set_line_width(LineWidth::try_from(0.38)?);
    let tint = visualizer.add_extra(MixPoint::Accent);
    visualizer.set_parameter(
        Parameter::ExtraSource(tint),
        SelectableInput::Style(StyleInput::PhaseRate).index() as f32,
    );
    let mut at = AudioTime::ZERO;
    for (model, name, seconds, accent) in [
        ("rif_ak47", "benga-ak47", 3.0, 7),
        ("pist_deagle", "benga-deagle", 4.0, 5),
        ("snip_awp", "benga-awp", 5.5, 3),
        ("eq_fraggrenade", "benga-grenade", 4.5, 2),
    ] {
        visualizer.set_parameter(Parameter::Style(StyleInput::PhaseRate), 1.0);
        visualizer.set_parameter(Parameter::ExtraGain(tint), accent as f32 / 11.0);
        select_model(&mut visualizer, model)?;
        for _ in 0..(seconds * 60.0) as usize {
            let end = at.shifted(1.0 / 60.0);
            visualizer.update(
                AudioInterval::new(at, end),
                ShutterFrames::default(),
                1.0 / 60.0,
                &palette,
                (WIDTH as f32, HEIGHT as f32),
                1.4,
                1.0,
            );
            at = end;
        }
        let original = visualizer.frame();
        ensure!(
            !original.model_lines.is_empty(),
            "{model} produced no wireframe"
        );
        // Exclude diagnostics; only the model's production depth and palette
        // shading belong in reusable brand artwork.
        let frame = Frame {
            lines: &[],
            ..original
        };
        let mut encoder = device.device.create_command_encoder(&Default::default());
        renderer.draw(
            device,
            &mut encoder,
            &background_view,
            &target_view,
            Size::new(WIDTH as f64, HEIGHT as f64),
            frame,
            None,
            None,
            1.0,
            None,
        );
        let stride = (WIDTH * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Brand readback"),
            size: u64::from(stride * HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(HEIGHT),
                },
            },
            extent,
        );
        device.queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        device.device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let mapped = readback.slice(..).get_mapped_range()?;
        let mut pixels: Vec<u8> = mapped
            .chunks(stride as usize)
            .flat_map(|row| row[..(WIDTH * 4) as usize].iter().copied())
            .collect();
        // Production additive passes preserve the background alpha. Encode
        // their emitted RGB with a max-channel matte for a reusable RGBA PNG.
        for pixel in pixels.chunks_exact_mut(4) {
            let alpha = u32::from(pixel[..3].iter().copied().max().unwrap());
            pixel[3] = alpha as u8;
            if alpha > 0 {
                for channel in &mut pixel[..3] {
                    *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        let path = output.join(format!("{name}.png"));
        write_cropped_png(&path, &pixels)?;
        drop(mapped);
        readback.unmap();
        println!(
            "{} ({model}, {} production edges)",
            path.display(),
            original.model_lines.len()
        );
    }
    Ok(())
}

fn select_model(visualizer: &mut Visualizer, id: &str) -> anyhow::Result<()> {
    // Tint is frozen when a model appears, so choose it after the inspector
    // accent setting is applied, even if the previous snapshot had this model.
    visualizer.navigate_model(Adjustment::Increase);
    visualizer.set_parameter(Parameter::Style(StyleInput::PhaseRate), 1.0);
    for _ in 0..128 {
        if visualizer.model_id() == Some(id) {
            return Ok(());
        }
        visualizer.navigate_model(Adjustment::Increase);
    }
    anyhow::bail!("Bundled model {id} was not found")
}

fn write_cropped_png(path: &Path, pixels: &[u8]) -> anyhow::Result<()> {
    let mut bounds = (WIDTH, HEIGHT, 0, 0);
    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
        if pixel[3] > 2 {
            let x = index as u32 % WIDTH;
            let y = index as u32 / WIDTH;
            bounds = (
                bounds.0.min(x),
                bounds.1.min(y),
                bounds.2.max(x + 1),
                bounds.3.max(y + 1),
            );
        }
    }
    ensure!(
        bounds.2 > bounds.0 && bounds.3 > bounds.1,
        "Wireframe is empty"
    );
    let left = bounds.0.saturating_sub(30);
    let top = bounds.1.saturating_sub(30);
    let right = (bounds.2 + 30).min(WIDTH);
    let bottom = (bounds.3 + 30).min(HEIGHT);
    let mut cropped = Vec::with_capacity(((right - left) * (bottom - top) * 4) as usize);
    for y in top..bottom {
        cropped.extend_from_slice(
            &pixels[((y * WIDTH + left) * 4) as usize..((y * WIDTH + right) * 4) as usize],
        );
    }
    let mut encoder = png::Encoder::new(
        BufWriter::new(File::create(path)?),
        right - left,
        bottom - top,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&cropped)?;
    Ok(())
}
