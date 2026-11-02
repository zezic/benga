//! Explicit integration check for machines with FFmpeg, ffprobe and a GPU:
//! cargo test --test render_video -- --ignored --nocapture

use std::{fs, path::Path, process::Command};

use anyhow::{ensure, Context};
use serde_json::Value;

fn write_audio(path: &Path, samples: u32) -> anyhow::Result<()> {
    let data_bytes = samples * 2 * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&48_000_u32.to_le_bytes());
    wav.extend_from_slice(&192_000_u32.to_le_bytes());
    wav.extend_from_slice(&4_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    for index in 0..samples {
        let sample = (0.25
            * (std::f64::consts::TAU * 440.0 * f64::from(index) / 48_000.0).sin()
            * f64::from(i16::MAX)) as i16;
        wav.extend_from_slice(&sample.to_le_bytes());
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(path, wav)?;
    Ok(())
}

#[test]
#[ignore = "requires FFmpeg with libx264/AAC, ffprobe and a compute-capable GPU"]
fn renders_fractional_fps_video_with_unscaled_audio_and_read_only_settings() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let input = directory.path().join("audio with spaces.wav");
    let output = directory.path().join("video with spaces.mp4");
    let config = directory.path().join("settings.ron");
    write_audio(&input, 12_001)?;
    let original_audio = fs::read(&input)?;
    // Older exposure preferences remain readable; the renderer uses its fixed
    // two-frame GPU shutter without modifying the saved source settings.
    let settings = "(preamp_db: 6.0, shutter_frames: 4.0, onboarding_seen: false)";
    fs::write(&config, settings)?;

    let result = Command::new(env!("CARGO_BIN_EXE_benga"))
        .arg("--render")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .args([
            "--fps", "29.97", "--width", "322", "--height", "180", "--gain", "-24",
        ])
        .arg("--config")
        .arg(&config)
        .output()?;
    ensure!(
        result.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let diagnostics = String::from_utf8_lossy(&result.stderr);
    assert!(diagnostics.contains("GPU ribbons"), "{diagnostics}");
    assert!(diagnostics.contains("shutter 2"), "{diagnostics}");
    assert_eq!(fs::read_to_string(&config)?, settings);
    assert_eq!(fs::read(&input)?, original_audio);

    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(&output)
        .output()
        .context("ffprobe must be on PATH")?;
    ensure!(probe.status.success(), "ffprobe failed");
    let metadata: Value = serde_json::from_slice(&probe.stdout)?;
    let streams = metadata["streams"].as_array().context("missing streams")?;
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .context("missing video")?;
    let audio = streams
        .iter()
        .find(|stream| stream["codec_type"] == "audio")
        .context("missing audio")?;
    assert_eq!(video["codec_name"], "h264");
    assert_eq!(video["width"], 322);
    assert_eq!(video["height"], 180);
    assert_eq!(video["avg_frame_rate"], "2997/100");
    assert_eq!(video["nb_read_frames"], "8");
    assert_eq!(audio["codec_name"], "aac");
    let duration: f64 = audio["duration"]
        .as_str()
        .context("missing audio duration")?
        .parse()?;
    assert!((duration - 12_001.0 / 48_000.0).abs() < 0.03);

    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&output)
        .args([
            "-map", "0:a:0", "-f", "f32le", "-ac", "2", "-ar", "48000", "pipe:1",
        ])
        .output()?;
    ensure!(
        decoded.status.success(),
        "could not decode exported soundtrack"
    );
    let levels: Vec<_> = decoded
        .stdout
        .chunks_exact(4)
        .map(|sample| f32::from_le_bytes(sample.try_into().unwrap()))
        .collect();
    let middle = &levels[2_000..levels.len() - 2_000];
    let rms = (middle
        .iter()
        .map(|&sample| f64::from(sample).powi(2))
        .sum::<f64>()
        / middle.len() as f64)
        .sqrt();
    assert!(
        (rms - 0.25 / 2.0_f64.sqrt()).abs() < 0.02,
        "--gain must leave soundtrack level unchanged: RMS {rms}"
    );

    // A second invocation must refuse to replace the completed video.
    let original_video = fs::read(&output)?;
    let rejected = Command::new(env!("CARGO_BIN_EXE_benga"))
        .arg("--render")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .arg("--config")
        .arg(&config)
        .output()?;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("--overwrite"));
    assert_eq!(fs::read(&output)?, original_video);
    Ok(())
}
