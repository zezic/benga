//! Resources embedded in every Benga executable.

pub(crate) struct EmbeddedModel {
    pub file_name: &'static str,
    pub bytes: &'static [u8],
}

// Generated from the sorted source directory so added/removed models enter the
// executable automatically, without a separately maintained filename list.
include!(concat!(env!("OUT_DIR"), "/embedded_models.rs"));

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
