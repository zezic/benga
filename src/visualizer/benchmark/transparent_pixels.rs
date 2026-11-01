//! GPU proof that omitting fully transparent model primitives preserves pixels.

use std::{
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context};

use super::super::{geometry::Vec2, Frame, LineInstance, ModelComposite, RibbonInstance};
use super::gpu::GpuBench;

fn pixels(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut reader = png::Decoder::new(BufReader::new(File::open(path)?)).read_info()?;
    let mut output = vec![
        0;
        reader
            .output_buffer_size()
            .context("PNG output size unavailable")?
    ];
    let info = reader.next_frame(&mut output)?;
    ensure!(
        info.width == 64 && info.height == 64,
        "unexpected pixel-check extent"
    );
    output.truncate(info.buffer_size());
    Ok(output)
}

#[test]
#[ignore = "requires a GPU; verifies additive and multiplicative pixel identity"]
fn transparent_model_geometry_can_be_omitted_without_changing_pixels() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var("BENGA_TRANSPARENT_PREVIEW_DIR")
            .unwrap_or_else(|_| "target/transparent-pixel-check".into()),
    );
    fs::create_dir_all(&directory)?;
    let visible = [0.8, 0.25, 0.1, 0.7];
    let clear = [1.0, 0.2, 0.9, 0.0];
    let lines = vec![
        LineInstance::flat(
            Vec2::new(8.0, 8.0),
            Vec2::new(56.0, 50.0),
            visible,
            visible,
            3.0,
        ),
        LineInstance::flat(
            Vec2::new(-100.0, -100.0),
            Vec2::new(164.0, 164.0),
            clear,
            clear,
            12.0,
        ),
        // One clear endpoint must not cause a genuinely visible edge to vanish.
        LineInstance::flat(
            Vec2::new(4.0, 32.0),
            Vec2::new(60.0, 32.0),
            clear,
            visible,
            5.0,
        ),
        LineInstance::flat(
            Vec2::new(0.0, 20.0),
            Vec2::new(64.0, 20.0),
            clear,
            clear,
            8.0,
        ),
    ];
    let ribbons = vec![
        RibbonInstance::triangle(
            [
                Vec2::new(6.0, 6.0),
                Vec2::new(60.0, 10.0),
                Vec2::new(30.0, 52.0),
            ],
            [
                [0.1, 0.6, 0.9, 0.15],
                [0.1, 0.6, 0.9, 0.5],
                [0.1, 0.6, 0.9, 0.05],
            ],
        )
        .coverage(3),
        // A zero-alpha, high-coverage triangle overlaps both visible primitives.
        RibbonInstance::triangle(
            [
                Vec2::new(-32.0, -32.0),
                Vec2::new(100.0, 0.0),
                Vec2::new(0.0, 100.0),
            ],
            [clear; 3],
        )
        .coverage(7),
        // Exercise the crossing-wing shader with two transparent vertices and
        // a visible peak, so filtering must test all three alpha components.
        RibbonInstance::wing_triangle(
            [
                Vec2::new(0.0, 63.0),
                Vec2::new(40.0, 63.0),
                Vec2::new(32.0, 12.0),
            ],
            [
                [0.3, 0.8, 1.0, 0.0],
                [0.3, 0.8, 1.0, 0.0],
                [0.3, 0.8, 1.0, 0.7],
            ],
        )
        .coverage(2),
        RibbonInstance::wing_triangle(
            [
                Vec2::new(12.0, 4.0),
                Vec2::new(64.0, 56.0),
                Vec2::new(-64.0, 56.0),
            ],
            [clear; 3],
        )
        .coverage(3),
    ];
    let selected_lines: Vec<_> = lines
        .iter()
        .copied()
        .filter(|line| line.start_color[3] != 0.0 || line.end_color[3] != 0.0)
        .collect();
    let selected_ribbons: Vec<_> = ribbons
        .iter()
        .copied()
        .filter(|ribbon| {
            ribbon.start_color[3] != 0.0
                || ribbon.end_color[3] != 0.0
                || ribbon.third_color[3] != 0.0
        })
        .collect();
    assert_eq!(selected_lines.len(), 2);
    assert_eq!(selected_ribbons.len(), 2);
    let mut gpu = GpuBench::new(64, 64)?;
    for (name, composite) in [
        ("additive", ModelComposite::Additive),
        ("multiplicative", ModelComposite::Multiplicative),
    ] {
        let before = directory.join(format!("{name}-before.png"));
        let after = directory.join(format!("{name}-after.png"));
        gpu.write_preview(
            Frame {
                lines: &[],
                model_lines: &lines,
                ribbons: &ribbons,
                model_composite: composite,
            },
            &before,
        )?;
        gpu.write_preview(
            Frame {
                lines: &[],
                model_lines: &selected_lines,
                ribbons: &selected_ribbons,
                model_composite: composite,
            },
            &after,
        )?;
        let expected = pixels(&before)?;
        let actual = pixels(&after)?;
        assert!(
            expected
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != expected[..3]),
            "{name} fixture must contain visible ink"
        );
        assert_eq!(
            actual, expected,
            "omitting zero-alpha {name} primitives must preserve every RGBA pixel"
        );
    }
    Ok(())
}
