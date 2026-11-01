//! Offline audio-file rendering. FFmpeg handles decoding and MP4 encoding;
//! analysis and drawing use the same pipeline as the desktop visualizer.

mod media;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{ensure, Context};
use vello::{kurbo::Size, Scene};

use crate::{
    audio::timeline::{AudioInterval, AudioTime},
    config::{Config, PreampDb},
    frame_schedule::FrameTime,
    render::OffscreenRenderer,
    visualizer::{Ui, View},
    AudioEvent,
};

const SAMPLE_RATE: usize = 48_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub input: PathBuf,
    pub output: PathBuf,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    /// Override the visualizer's preamp, leaving the encoded audio unchanged.
    pub gain_db: Option<f32>,
    pub overwrite: bool,
    pub config: Option<PathBuf>,
}

impl Options {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.fps.is_finite() && (10.0..=240.0).contains(&self.fps),
            "FPS must be finite and between 10 and 240"
        );
        ensure!(
            self.width > 0 && self.height > 0 && self.width % 2 == 0 && self.height % 2 == 0,
            "MP4 width and height must be positive even numbers"
        );
        ensure!(
            self.output
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4")),
            "the output filename must end in .mp4"
        );
        if let Some(db) = self.gain_db {
            PreampDb::try_from(db).context("invalid --gain")?;
        }
        Ok(())
    }
}

/// Render as fast as the GPU and encoder allow, using a fixed media clock.
/// The caller supplies settings read from disk; rendering never saves them.
pub fn run(options: Options, config: &Config) -> anyhow::Result<()> {
    options.validate()?;
    let input = options
        .input
        .canonicalize()
        .with_context(|| format!("Could not open audio input {}", options.input.display()))?;
    ensure!(input.is_file(), "audio input must be a file");
    let destination = media::Destination::new(&input, &options.output, options.overwrite)?;
    let mut decoder = media::Decoder::new(&input)?;
    let mut samples = decoder.read_samples(sample_boundary(1, options.fps) as usize)?;
    ensure!(
        !samples.is_empty(),
        "the input contains no decodable audio samples"
    );

    let mut render = OffscreenRenderer::new(options.width, options.height)?;
    let mut ui = Ui::new(config)?;
    if let Some(db) = options.gain_db {
        ui.set_preamp(PreampDb::try_from(db)?);
    }
    ui.dismiss_startup();
    ui.set_plate_mask(Some(render.plate_mask_image()));
    ui.audio_event(AudioEvent::Config {
        samplerate: SAMPLE_RATE,
    });
    let mut view = View::fit(Size::new(
        f64::from(options.width),
        f64::from(options.height),
    ));
    view.set_zoom(config.zoom);
    let mut encoder = media::Encoder::new(&input, destination.temporary_path(), &options)?;
    let mut scene = Scene::new();
    let origin = Instant::now();
    let mut progress = origin;
    let mut fed_samples = 0_u64;
    let mut frames = 0_u64;
    eprintln!(
        "Rendering {} at {}x{}, {} FPS, analyzer gain {} dB",
        input.display(),
        options.width,
        options.height,
        options.fps,
        options.gain_db.unwrap_or(config.preamp_db.db()),
    );
    loop {
        ui.consume_buf(
            &samples,
            origin + Duration::from_secs_f64(fed_samples as f64 / SAMPLE_RATE as f64),
        );
        fed_samples += samples.len() as u64;
        let start = frames as f64 / options.fps;
        let end = (frames + 1) as f64 / options.fps;
        scene.reset();
        let frame = ui.draw_at_audio_time(
            &mut scene,
            &view,
            FrameTime::new(
                origin + Duration::from_secs_f64(start),
                origin + Duration::from_secs_f64(end),
            ),
            AudioInterval::new(AudioTime::ZERO.shifted(start), AudioTime::ZERO.shifted(end)),
        );
        let pixels = render.render(&scene, frame)?;
        encoder.write_frame(&pixels)?;
        frames += 1;
        if progress.elapsed() >= Duration::from_secs(5) {
            eprintln!("Rendered {frames} frames ({end:.1} seconds of video)");
            progress = Instant::now();
        }
        let due = sample_boundary(frames + 1, options.fps);
        samples = decoder.read_samples((due - fed_samples) as usize)?;
        if samples.is_empty() {
            break;
        }
    }
    decoder.finish()?;
    encoder.finish()?;
    destination.publish(options.overwrite)?;
    eprintln!("Rendered {frames} frames to {}", options.output.display());
    Ok(())
}

// Derive every boundary from the absolute frame index to avoid cumulative
// rounding drift at fractional FPS or rates that do not divide 48 kHz.
fn sample_boundary(frame: u64, fps: f64) -> u64 {
    (frame as f64 * SAMPLE_RATE as f64 / fps).ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_boundaries_cover_audio_without_accumulating_rounding_error() {
        for fps in [10.0, 24.0, 29.97, 59.0, 60.0, 239.76, 240.0] {
            let mut covered = 0;
            for frame in 1..=10_000 {
                let boundary = sample_boundary(frame, fps);
                let count = boundary - covered;
                assert!((200..=4_800).contains(&count));
                let time = boundary as f64 / SAMPLE_RATE as f64;
                assert!(time + 1e-10 >= frame as f64 / fps);
                assert!(time - frame as f64 / fps < 1.0 / SAMPLE_RATE as f64 + 1e-10);
                covered += count;
            }
        }
    }
}
