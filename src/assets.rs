//! Resolve bundled resources independently of the shell's working directory.
use std::path::{Path, PathBuf};

pub fn path(relative: impl AsRef<Path>) -> PathBuf {
    let executable = std::env::current_exe().ok();
    let directory = executable.as_deref().and_then(Path::parent);
    let mut roots = Vec::new();
    if let Some(directory) = directory {
        roots.push(directory.join("../Resources/assets"));
        roots.push(directory.join("assets"));
        roots.push(directory.join("../lib/benga/assets"));
    }
    // Development only: installed binaries must not load assets from an arbitrary cwd.
    if cfg!(test)
        || cfg!(debug_assertions)
        || directory
            .is_some_and(|p| p.starts_with(Path::new(env!("CARGO_MANIFEST_DIR")).join("target")))
    {
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    }
    roots
        .into_iter()
        .map(|root| root.join(relative.as_ref()))
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| {
            directory
                .unwrap_or(Path::new("."))
                .join("assets")
                .join(relative)
        })
}

pub fn window_icon() -> Option<winit::window::Icon> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(include_bytes!(
        "../assets/icons/benga-256.png"
    )))
    .read_info()
    .ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    pixels.truncate(info.buffer_size());
    winit::window::Icon::from_rgba(pixels, info.width, info.height).ok()
}
