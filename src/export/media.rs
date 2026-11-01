use std::{
    fs::{self, File},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use anyhow::{bail, ensure, Context};
use tempfile::NamedTempFile;

use super::Options;

/// Encode beside the destination so publication is atomic and failed runs
/// leave existing videos untouched, even when overwrite was requested.
pub(super) struct Destination {
    temporary: NamedTempFile,
    path: PathBuf,
}

impl Destination {
    pub fn new(input: &Path, output: &Path, overwrite: bool) -> anyhow::Result<Self> {
        let absolute = std::env::current_dir()?.join(output);
        let parent = absolute
            .parent()
            .context("output must name a file")?
            .canonicalize()
            .context("the output directory must exist")?;
        let path = parent.join(absolute.file_name().context("output must name a file")?);
        ensure!(
            path.canonicalize().ok().as_deref() != Some(input),
            "input and output must be different files"
        );
        if fs::symlink_metadata(&path).is_ok() {
            ensure!(
                overwrite,
                "output {} already exists; use --overwrite to replace it",
                path.display()
            );
            ensure!(!path.is_dir(), "output must name a file");
        }
        let temporary = tempfile::Builder::new()
            .prefix(".benga-render-")
            .suffix(".mp4")
            .tempfile_in(parent)
            .context("could not create a temporary video in the output directory")?;
        Ok(Self { temporary, path })
    }

    pub fn temporary_path(&self) -> &Path {
        self.temporary.path()
    }

    pub fn publish(self, overwrite: bool) -> anyhow::Result<()> {
        if overwrite {
            self.temporary.persist(&self.path)
        } else {
            self.temporary.persist_noclobber(&self.path)
        }
        .map_err(|error| error.error)
        .with_context(|| format!("could not save video to {}", self.path.display()))?;
        Ok(())
    }
}

/// Stderr goes to a temporary file, avoiding pipe deadlocks and unbounded RAM.
/// Every early return also terminates and reaps the child process.
struct Ffmpeg {
    child: Option<Child>,
    diagnostics: File,
    operation: &'static str,
}

impl Ffmpeg {
    fn spawn(command: &mut Command, operation: &'static str) -> anyhow::Result<Self> {
        let diagnostics = tempfile::tempfile()?;
        let child = command
            .stderr(Stdio::from(diagnostics.try_clone()?))
            .spawn()
            .context("Could not start FFmpeg; install ffmpeg and make it available on PATH")?;
        Ok(Self {
            child: Some(child),
            diagnostics,
            operation,
        })
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let status = child.wait().context("could not wait for FFmpeg")?;
        if !status.success() {
            let length = self.diagnostics.metadata()?.len().min(16_384);
            self.diagnostics.seek(SeekFrom::End(-(length as i64)))?;
            let mut bytes = Vec::new();
            self.diagnostics.read_to_end(&mut bytes)?;
            bail!(
                "FFmpeg {} failed ({status}): {}",
                self.operation,
                String::from_utf8_lossy(&bytes).trim()
            );
        }
        Ok(())
    }
}

impl Drop for Ffmpeg {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(super) struct Decoder {
    process: Ffmpeg,
    output: BufReader<ChildStdout>,
}

impl Decoder {
    pub fn new(input: &Path) -> anyhow::Result<Self> {
        let mut command = Command::new("ffmpeg");
        command
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(input)
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-sn",
                "-dn",
                "-ac",
                "2",
                "-ar",
                "48000",
                "-c:a",
                "pcm_f32le",
                "-f",
                "f32le",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let mut process = Ffmpeg::spawn(&mut command, "audio decoding")?;
        let output = BufReader::new(process.child.as_mut().unwrap().stdout.take().unwrap());
        Ok(Self { process, output })
    }

    pub fn read_samples(&mut self, frames: usize) -> anyhow::Result<Vec<[f32; 2]>> {
        let (samples, eof) = read_pcm(&mut self.output, frames)?;
        if eof {
            self.process.finish()?;
        }
        Ok(samples)
    }

    pub fn finish(&mut self) -> anyhow::Result<()> {
        self.process.finish()
    }
}

fn read_pcm(input: &mut impl Read, frames: usize) -> anyhow::Result<(Vec<[f32; 2]>, bool)> {
    let mut bytes = vec![0_u8; frames * 8];
    let mut used = 0;
    let mut eof = false;
    while used < bytes.len() {
        match input.read(&mut bytes[used..]) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(count) => used += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("could not read decoded audio"),
        }
    }
    ensure!(
        used % 8 == 0,
        "decoded audio ends in an incomplete stereo sample"
    );
    let mut samples = Vec::with_capacity(used / 8);
    for frame in bytes[..used].chunks_exact(8) {
        let left = f32::from_le_bytes(frame[..4].try_into().unwrap());
        let right = f32::from_le_bytes(frame[4..].try_into().unwrap());
        ensure!(
            left.is_finite() && right.is_finite(),
            "decoded audio contains a non-finite sample"
        );
        samples.push([left, right]);
    }
    Ok((samples, eof))
}

pub(super) struct Encoder {
    process: Ffmpeg,
    input: Option<ChildStdin>,
}

impl Encoder {
    pub fn new(audio: &Path, output: &Path, options: &Options) -> anyhow::Result<Self> {
        let mut command = Command::new("ffmpeg");
        // The finite video pipe already covers the decoded audio duration.
        // `-shortest` can discard its final partial frame during AAC muxing.
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgba",
                "-video_size",
            ])
            .arg(format!("{}x{}", options.width, options.height))
            .arg("-framerate")
            .arg(options.fps.to_string())
            .args(["-i", "pipe:0", "-i"])
            .arg(audio)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "libx264",
                "-preset",
                "medium",
                "-crf",
                "18",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "320k",
                "-af",
                "asetpts=PTS-STARTPTS",
                "-movflags",
                "+faststart",
                "-f",
                "mp4",
            ])
            .arg(output)
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
        let mut process = Ffmpeg::spawn(&mut command, "video encoding")?;
        let input = process.child.as_mut().unwrap().stdin.take();
        Ok(Self { process, input })
    }

    pub fn write_frame(&mut self, pixels: &[u8]) -> anyhow::Result<()> {
        if let Err(error) = self
            .input
            .as_mut()
            .context("video encoder is closed")?
            .write_all(pixels)
        {
            self.input.take();
            self.process.finish()?;
            return Err(error).context("could not send a video frame to FFmpeg");
        }
        Ok(())
    }

    pub fn finish(&mut self) -> anyhow::Result<()> {
        self.input.take();
        self.process.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn pcm_preserves_stereo_and_final_partial_blocks() -> anyhow::Result<()> {
        let bytes: Vec<_> = [0.25_f32, -0.5, 0.75, -1.0, 0.0, 0.125]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let mut input = Cursor::new(bytes);
        assert_eq!(
            read_pcm(&mut input, 2)?,
            (vec![[0.25, -0.5], [0.75, -1.0]], false)
        );
        assert_eq!(read_pcm(&mut input, 2)?, (vec![[0.0, 0.125]], true));
        assert_eq!(read_pcm(&mut input, 2)?, (vec![], true));
        assert!(read_pcm(&mut Cursor::new([0_u8; 7]), 1).is_err());
        assert!(read_pcm(
            &mut Cursor::new(
                [f32::NAN, 0.0]
                    .into_iter()
                    .flat_map(f32::to_le_bytes)
                    .collect::<Vec<_>>()
            ),
            1
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn publication_protects_input_existing_outputs_and_concurrent_creation() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let input = directory.path().join("input.mp4");
        let output = directory.path().join("output.mp4");
        fs::write(&input, b"audio")?;
        let input = input.canonicalize()?;
        assert!(Destination::new(&input, &input, true).is_err());
        let first = Destination::new(&input, &output, false)?;
        fs::write(first.temporary_path(), b"video")?;
        fs::write(&output, b"existing")?;
        assert!(first.publish(false).is_err());
        assert_eq!(fs::read(&output)?, b"existing");
        assert!(Destination::new(&input, &output, false).is_err());
        let second = Destination::new(&input, &output, true)?;
        fs::write(second.temporary_path(), b"replacement")?;
        second.publish(true)?;
        assert_eq!(fs::read(&output)?, b"replacement");
        assert_eq!(fs::read(&input)?, b"audio");
        Ok(())
    }
}
