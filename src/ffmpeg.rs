//! Media in and out through the `ffmpeg` / `ffprobe` executables on PATH. No codec libraries to link.

use crate::{SRC_H, SRC_W};
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// What ffprobe knows about an input.
#[derive(Debug, Clone)]
pub struct Probe { pub width: u32, pub height: u32, pub fps: f64, pub frames: Option<u64>, pub has_audio: bool, pub duration: Option<f64> }

pub fn check_tools() -> io::Result<()> {
    for tool in ["ffmpeg", "ffprobe"] {
        Command::new(tool).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status()
            .map_err(|e| io::Error::new(e.kind(), format!("{tool} not found on PATH ({e}). Install ffmpeg and try again.")))?;
    }
    Ok(())
}

pub fn probe(path: &str) -> io::Result<Probe> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,width,height,r_frame_rate,avg_frame_rate,nb_frames:format=duration",
               "-of", "default=noprint_wrappers=1", path])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("ffprobe couldn't read {path}: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut width, mut height, mut fps, mut frames, mut has_audio, mut duration) = (0u32, 0u32, 0.0f64, None, false, None);
    let mut in_video = false;
    for line in text.lines() {
        let (k, v) = match line.split_once('=') { Some(kv) => kv, None => continue };
        match k {
            "codec_type" => { in_video = v == "video"; if v == "audio" { has_audio = true; } }
            "width" if in_video && width == 0 => width = v.parse().unwrap_or(0),
            "height" if in_video && height == 0 => height = v.parse().unwrap_or(0),
            "avg_frame_rate" | "r_frame_rate" if in_video && fps == 0.0 => fps = parse_rate(v),
            "nb_frames" if in_video && frames.is_none() => frames = v.parse().ok(),
            "duration" => duration = v.parse().ok(),
            _ => {}
        }
    }
    if width == 0 || height == 0 { return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("no video stream in {path}"))); }
    Ok(Probe { width, height, fps, frames, has_audio, duration })
}

fn parse_rate(s: &str) -> f64 {
    match s.split_once('/') {
        Some((a, b)) => { let (a, b): (f64, f64) = (a.parse().unwrap_or(0.0), b.parse().unwrap_or(1.0)); if b == 0.0 { 0.0 } else { a / b } }
        None => s.parse().unwrap_or(0.0),
    }
}

/// Decodes any ffmpeg-readable file to 640×480 RGB8 frames (letterboxed to 4:3) at a fixed rate,
/// delivered **bottom row first** to match the filter's GL convention.
pub struct Decoder { child: Child, stdout: ChildStdout, buf: Vec<u8>, pub fps: f64 }

impl Decoder {
    /// `fps` is the output rate (29.97 for one frame per NTSC frame, 59.94 to get one per field; 0 = native, one frame).
    /// `start` seeks before decoding; `loop_input` repeats the file forever (for the player).
    pub fn open(path: &str, fps: f64, start: f64, loop_input: bool) -> io::Result<Decoder> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "fatal", "-nostdin"]);
        if loop_input { cmd.args(["-stream_loop", "-1"]); }
        if start > 0.0 { cmd.args(["-ss", &format!("{start}")]); }
        let rate = if fps > 0.0 { format!("fps={fps},") } else { String::new() };
        cmd.args(["-i", path, "-an", "-sn", "-frames:v", if fps > 0.0 { "2147483647" } else { "1" },
                  "-vf", &format!("{rate}scale={SRC_W}:{SRC_H}:force_original_aspect_ratio=decrease:flags=bicubic,pad={SRC_W}:{SRC_H}:(ow-iw)/2:(oh-ih)/2:black,vflip,format=rgb24"),
                  "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?;
        let stdout = child.stdout.take().unwrap();
        Ok(Decoder { child, stdout, buf: vec![0; SRC_W * SRC_H * 3], fps })
    }

    /// Next frame, or None at end of stream.
    pub fn next(&mut self) -> io::Result<Option<&[u8]>> {
        match self.stdout.read_exact(&mut self.buf) {
            Ok(()) => Ok(Some(&self.buf)),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

/// Loads a still image as a 640×480 RGB8 frame, letterboxed, bottom row first.
pub fn load_image(path: &str) -> io::Result<Vec<u8>> {
    let mut d = Decoder::open(path, 0.0, 0.0, false)?;
    match d.next()? { Some(f) => Ok(f.to_vec()), None => Err(io::Error::new(io::ErrorKind::InvalidData, format!("couldn't decode an image from {path}"))) }
}

/// Encodes RGB8 frames (top row first) to a file. H.264 in MP4 by default, with the source's audio
/// copied across when `audio_from` is given. Frame size is fixed by the first call.
pub struct Encoder { child: Child, stdin: Option<ChildStdin>, pub frames: u64 }

impl Encoder {
    pub fn open(path: &str, w: usize, h: usize, fps: f64, audio_from: Option<(&str, f64)>, crf: u32) -> io::Result<Encoder> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-nostdin", "-y",
                  "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{w}x{h}"), "-r", &format!("{fps}"), "-i", "pipe:0"]);
        if let Some((src, start)) = audio_from {
            if start > 0.0 { cmd.args(["-ss", &format!("{start}")]); }
            cmd.args(["-i", src, "-map", "0:v:0", "-map", "1:a:0?", "-c:a", "aac", "-b:a", "192k", "-shortest"]);
        }
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".gif") {
            cmd.args(["-vf", "split[a][b];[a]palettegen=max_colors=128[p];[b][p]paletteuse=dither=bayer", "-loop", "0"]);
        } else if lower.ends_with(".webm") {
            cmd.args(["-c:v", "libvpx-vp9", "-b:v", "0", "-crf", &format!("{crf}"), "-pix_fmt", "yuv420p"]);
        } else {
            // yuv420p + 4:3 display aspect: the same kind of file the real cards wrote to disk
            cmd.args(["-c:v", "libx264", "-preset", "medium", "-crf", &format!("{crf}"), "-pix_fmt", "yuv420p", "-aspect", "4:3"]);
            if lower.ends_with(".mp4") || lower.ends_with(".mov") || lower.ends_with(".m4v") { cmd.args(["-movflags", "+faststart"]); }
        }
        cmd.arg(path);
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::inherit()).spawn()?;
        let stdin = child.stdin.take();
        Ok(Encoder { child, stdin, frames: 0 })
    }

    pub fn write(&mut self, rgb: &[u8]) -> io::Result<()> {
        self.stdin.as_mut().expect("encoder already finished").write_all(rgb)?;
        self.frames += 1;
        Ok(())
    }

    /// Closes the pipe and waits for ffmpeg to finish the file.
    pub fn finish(mut self) -> io::Result<u64> {
        drop(self.stdin.take());
        let status = self.child.wait()?;
        if !status.success() { return Err(io::Error::other("ffmpeg failed to write the output file")); }
        Ok(self.frames)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) { drop(self.stdin.take()); let _ = self.child.wait(); }
}

/// Writes an RGB8 image (top row first) as PNG.
pub fn save_png(path: &str, w: usize, h: usize, rgb: &[u8]) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(io::Error::other)?;
    writer.write_image_data(rgb).map_err(io::Error::other)?;
    Ok(())
}
