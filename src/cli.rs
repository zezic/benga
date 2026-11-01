use std::{ffi::OsString, path::PathBuf, str::FromStr};

use anyhow::{bail, ensure, Context};
use benga::{config::PreampDb, export::Options};

pub enum Command {
    Desktop,
    Profile,
    Render(Options),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<Command> {
    let args: Vec<_> = args.into_iter().collect();
    if args.is_empty() {
        return Ok(Command::Desktop);
    }
    if args.len() == 1 {
        match args[0].to_str() {
            Some("--help" | "-h") => return Ok(Command::Help),
            Some("--version") => return Ok(Command::Version),
            Some("--profile") => return Ok(Command::Profile),
            _ => {}
        }
    }
    let mut input = None;
    let mut output = None;
    let mut fps = None;
    let mut width = None;
    let mut height = None;
    let mut gain_db = None;
    let mut overwrite = false;
    let mut config = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--render") => {
                set_once(&mut input, value(&mut args, "--render")?.into(), "--render")?
            }
            Some("--output") => set_once(
                &mut output,
                value(&mut args, "--output")?.into(),
                "--output",
            )?,
            Some("--fps") => set_once(&mut fps, number(&mut args, "--fps")?, "--fps")?,
            Some("--width") => set_once(&mut width, number(&mut args, "--width")?, "--width")?,
            Some("--height") => set_once(&mut height, number(&mut args, "--height")?, "--height")?,
            Some("--gain") => set_once(&mut gain_db, number(&mut args, "--gain")?, "--gain")?,
            Some("--config") => set_once(
                &mut config,
                PathBuf::from(value(&mut args, "--config")?),
                "--config",
            )?,
            Some("--overwrite") => {
                ensure!(!overwrite, "--overwrite may only be specified once");
                overwrite = true;
            }
            _ => bail!(
                "Unknown or conflicting argument: {}. Run benga --help for usage.",
                arg.to_string_lossy()
            ),
        }
    }
    let input = input.context("--render AUDIO is required for video export")?;
    let output = output.context("--output VIDEO.mp4 is required with --render")?;
    let fps: f64 = fps.unwrap_or(60.0);
    ensure!(
        fps.is_finite() && (10.0..=240.0).contains(&fps),
        "--fps must be finite and between 10 and 240"
    );
    let width = width.unwrap_or(1920);
    let height = height.unwrap_or(1080);
    ensure!(
        width > 0 && width % 2 == 0,
        "--width must be a positive even integer"
    );
    ensure!(
        height > 0 && height % 2 == 0,
        "--height must be a positive even integer"
    );
    if let Some(gain) = gain_db {
        PreampDb::try_from(gain).context("Invalid --gain")?;
    }
    Ok(Command::Render(Options {
        input,
        output,
        fps,
        width,
        height,
        gain_db,
        overwrite,
        config,
    }))
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> anyhow::Result<()> {
    ensure!(slot.is_none(), "{flag} may only be specified once");
    *slot = Some(value);
    Ok(())
}

fn value(args: &mut impl Iterator<Item = OsString>, flag: &str) -> anyhow::Result<OsString> {
    let value = args
        .next()
        .with_context(|| format!("{flag} requires a value"))?;
    ensure!(
        !value.is_empty() && !value.to_string_lossy().starts_with("--"),
        "{flag} requires a value"
    );
    Ok(value)
}

fn number<T: FromStr>(args: &mut impl Iterator<Item = OsString>, flag: &str) -> anyhow::Result<T> {
    value(args, flag)?
        .to_str()
        .and_then(|text| text.parse().ok())
        .with_context(|| format!("{flag} requires a valid number"))
}

pub fn help() {
    println!(
        "Benga {} — desktop audio visualizer
Usage: benga [--version | --help | --profile]
       benga --render AUDIO --output VIDEO.mp4 [OPTIONS]

Render options:
  --fps NUMBER       Frames per second, 10–240; decimals accepted (default: 60)
  --width PIXELS     Positive even output width (default: 1920)
  --height PIXELS    Positive even output height (default: 1080)
  --gain DB          Analyzer gain in decibels, -96 to 96 (default: saved gain)
  --config FILE      Read settings from this RON file instead of saved settings
  --overwrite        Replace an existing output video

Rendering requires FFmpeg on PATH and a compute-capable GPU. Runs without a
window or audio capture. Exports MP4 with the source audio; --gain changes the
visualizer's sensitivity and leaves the soundtrack volume unchanged.

In the app: F1 for audio help and updates, double-click for fullscreen, Esc to leave.
https://benga.zezic.ru • https://github.com/zezic/benga",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(args: &[&str]) -> anyhow::Result<Command> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn preserves_desktop_profile_and_information_commands() -> anyhow::Result<()> {
        assert!(matches!(parse_text(&[])?, Command::Desktop));
        assert!(matches!(parse_text(&["--profile"])?, Command::Profile));
        assert!(matches!(parse_text(&["--help"])?, Command::Help));
        assert!(matches!(parse_text(&["-h"])?, Command::Help));
        assert!(matches!(parse_text(&["--version"])?, Command::Version));
        Ok(())
    }

    #[test]
    fn render_defaults_and_negative_gain() -> anyhow::Result<()> {
        let Command::Render(defaults) =
            parse_text(&["--render", "song with spaces.flac", "--output", "video.mp4"])?
        else {
            panic!("expected render")
        };
        assert_eq!(defaults.input, PathBuf::from("song with spaces.flac"));
        assert_eq!(
            (defaults.fps, defaults.width, defaults.height),
            (60.0, 1920, 1080)
        );
        assert_eq!(defaults.gain_db, None);
        assert!(!defaults.overwrite);
        let Command::Render(custom) = parse_text(&[
            "--fps",
            "29.97",
            "--render",
            "song.wav",
            "--output",
            "video.mp4",
            "--width",
            "1280",
            "--height",
            "720",
            "--gain",
            "-12.5",
            "--config",
            "render.ron",
            "--overwrite",
        ])?
        else {
            panic!("expected render")
        };
        assert_eq!(
            (custom.fps, custom.width, custom.height),
            (29.97, 1280, 720)
        );
        assert_eq!(custom.gain_db, Some(-12.5));
        assert_eq!(custom.config, Some(PathBuf::from("render.ron")));
        assert!(custom.overwrite);
        Ok(())
    }

    #[test]
    fn rejects_invalid_or_ambiguous_export_arguments() {
        for args in [
            vec!["--render", "song.wav"],
            vec!["--output", "video.mp4"],
            vec!["--render", "--output", "video.mp4"],
            vec!["--render", "song.wav", "--output"],
            vec!["--render", "song.wav", "--output", "video.mp4", "--profile"],
            vec!["--help", "--profile"],
            vec!["--version", "extra"],
        ] {
            assert!(parse_text(&args).is_err(), "{args:?}");
        }
        for extra in [
            vec!["--fps", "NaN"],
            vec!["--fps", "inf"],
            vec!["--fps", "0"],
            vec!["--fps", "9.9"],
            vec!["--fps", "241"],
            vec!["--width", "0"],
            vec!["--width", "1919"],
            vec!["--height", "-2"],
            vec!["--height", "abc"],
            vec!["--gain", "NaN"],
            vec!["--gain", "97"],
            vec!["--gain", "-97"],
            vec!["--fps", "60", "--fps", "30"],
            vec!["--render", "other.wav"],
            vec!["--overwrite", "--overwrite"],
            vec!["--mystery"],
        ] {
            let mut args = vec!["--render", "song.wav", "--output", "video.mp4"];
            args.extend(extra);
            assert!(parse_text(&args).is_err(), "{args:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn accepts_non_utf8_audio_paths() -> anyhow::Result<()> {
        use std::os::unix::ffi::OsStringExt;
        let input = OsString::from_vec(b"song\xff.wav".to_vec());
        let Command::Render(options) = parse([
            OsString::from("--render"),
            input.clone(),
            OsString::from("--output"),
            OsString::from("video.mp4"),
        ])?
        else {
            panic!("expected render")
        };
        assert_eq!(options.input, PathBuf::from(input));
        Ok(())
    }
}
