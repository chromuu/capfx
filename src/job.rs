//! Shared by the command line and the GUI: field pairing from a decoder, and whole-file rendering.

use crate::ffmpeg::{self, Decoder, Encoder, Probe};
use crate::{present, Params, Pipeline, SRC_H, SRC_W};
use std::io;
use std::time::Instant;

/// Yields (earlier, later) console frames for each NTSC frame. Sources at 50 fps or more give one
/// picture per field; slower ones use consecutive frames as the two fields, like a real capture.
pub struct Fields { dec: Decoder, per_field: bool, pub prev: Vec<u8>, pub cur: Vec<u8>, started: bool }

impl Fields {
    pub fn open(path: &str, probe: &Probe, start: f64, loop_input: bool) -> io::Result<Fields> {
        let per_field = probe.fps >= 50.0;
        let dec = Decoder::open(path, if per_field { 59.94 } else { 29.97 }, start, loop_input)?;
        Ok(Fields { dec, per_field, prev: vec![0; SRC_W * SRC_H * 3], cur: vec![0; SRC_W * SRC_H * 3], started: false })
    }
    /// Advances one NTSC frame; false at end of input.
    pub fn advance(&mut self) -> io::Result<bool> {
        if self.per_field {
            match self.dec.next()? { Some(f) => self.prev.copy_from_slice(f), None => return Ok(false) }
            match self.dec.next()? { Some(f) => self.cur.copy_from_slice(f), None => self.cur.copy_from_slice(&self.prev) }
        } else {
            std::mem::swap(&mut self.prev, &mut self.cur);
            match self.dec.next()? { Some(f) => self.cur.copy_from_slice(f), None => return Ok(false) }
            if !self.started { self.prev.copy_from_slice(&self.cur); }
        }
        self.started = true;
        Ok(true)
    }
}

/// True when ffprobe says this is a single picture rather than a video.
pub fn is_image(path: &str, probe: &Probe) -> bool {
    let l = path.to_ascii_lowercase();
    probe.frames == Some(1) || probe.fps == 0.0 || [".png", ".jpg", ".jpeg", ".bmp", ".webp", ".tif", ".tiff"].iter().any(|e| l.ends_with(e))
}

#[derive(Clone, Debug)]
pub struct RenderOpts {
    pub input: String,
    pub output: String,
    pub params: Params,
    pub size: (usize, usize),
    pub start: f64,
    /// Output frames to write; None = whole file (150 frames for a still image).
    pub frames: Option<u64>,
    pub crf: u32,
    pub audio: bool,
}

/// Processes a whole file. `progress(done, total)` is called every frame; return false to cancel.
/// Returns the number of frames written (0 if cancelled, and the partial file is removed).
pub fn render_file(o: &RenderOpts, progress: &mut dyn FnMut(u64, Option<u64>) -> bool) -> io::Result<u64> {
    ffmpeg::check_tools()?;
    let probe = ffmpeg::probe(&o.input)?;
    let (w, h) = o.size;
    let image = is_image(&o.input, &probe);
    let audio = if o.audio && probe.has_audio && !image { Some((o.input.as_str(), o.start)) } else { None };
    let total = match (o.frames, image, probe.duration) {
        (Some(n), _, _) => Some(n),
        (None, true, _) => Some(150),
        (None, false, Some(d)) => Some((((d - o.start).max(0.0)) * 29.97).round() as u64),
        _ => None,
    };
    let mut enc = Encoder::open(&o.output, w, h, 29.97, audio, o.crf)?;
    let mut pl = Pipeline::new();
    let mut n = 0u64;
    let mut cancelled = false;
    if image {
        let img = ffmpeg::load_image(&o.input)?;
        while n < total.unwrap_or(150) {
            pl.process(&o.params, &img, &img);
            enc.write(&present::render(&pl, o.params.view, o.params.overscan, None, w, h))?;
            n += 1;
            if !progress(n, total) { cancelled = true; break; }
        }
    } else {
        let mut fields = Fields::open(&o.input, &probe, o.start, false)?;
        while o.frames.is_none_or(|l| n < l) && fields.advance()? {
            pl.process(&o.params, &fields.prev, &fields.cur);
            enc.write(&present::render(&pl, o.params.view, o.params.overscan, None, w, h))?;
            n += 1;
            if !progress(n, total) { cancelled = true; break; }
        }
    }
    if cancelled { drop(enc); let _ = std::fs::remove_file(&o.output); return Ok(0); }
    enc.finish()
}

/// Time-stamped file name like `capfx_20260101_120000.png` in the current directory.
pub fn stamped(ext: &str) -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86400) as i64; let rem = secs % 86400;
    let z = days + 719468; let era = z.div_euclid(146097); let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("capfx_{y:04}{m:02}{d:02}_{:02}{:02}{:02}.{ext}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Simple rate meter for the status line.
pub struct FpsMeter { t0: Instant, n: u32, pub fps: f32 }
impl Default for FpsMeter { fn default() -> Self { FpsMeter { t0: Instant::now(), n: 0, fps: 0.0 } } }
impl FpsMeter {
    pub fn tick(&mut self) {
        self.n += 1;
        let e = self.t0.elapsed().as_secs_f32();
        if e >= 1.0 { self.fps = self.n as f32 / e; self.n = 0; self.t0 = Instant::now(); }
    }
}
