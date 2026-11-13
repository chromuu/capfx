//! 2005-era composite capture card simulation, CPU implementation.
//!
//! A straight port of the tested WebGL2 shader pipeline:
//! console frame → interlaced 720×480 raster → NTSC composite at 4fsc → notch / comb Y/C separation →
//! quadrature demodulation → BT.601 4:2:0 → MPEG-2 intra quantisation → interlaced playback.
//!
//! Everything works in GL image convention: row 0 is the **bottom** of the picture, matching the
//! shader source so the two implementations can be compared pixel for pixel.

use rayon::prelude::*;
use std::f32::consts::PI;

pub mod audio;
pub mod ffmpeg;
pub mod gui;
pub mod job;
pub mod present;

/// Console frame size the filter expects (4:3).
pub const SRC_W: usize = 640;
pub const SRC_H: usize = 480;
/// Capture raster (BT.601 525-line).
pub const CAP_W: usize = 720;
pub const CAP_H: usize = 480;
/// Composite samples per line at 4fsc (active portion).
pub const COMP_W: usize = 764;
/// Gaussian sigma in 4fsc samples = MHZ / (-3 dB frequency in MHz).
const MHZ: f32 = 1.897;

// ------------------------------------------------------------------------------------------ params

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YcMode { Notch = 0, Comb2 = 1, Comb3 = 2 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Levels { Matched = 0, RaisedBlacks = 1, CrushedBlacks = 2 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deint { Weave = 0, Bob = 1, Blend = 2 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View { Source, Composite, Luma, Chroma, Decoded, Final }

impl View {
    pub const ALL: [View; 6] = [View::Source, View::Composite, View::Luma, View::Chroma, View::Decoded, View::Final];
    pub fn name(self) -> &'static str {
        match self { View::Source => "source", View::Composite => "composite", View::Luma => "luma",
                     View::Chroma => "chroma", View::Decoded => "decoded", View::Final => "capture" }
    }
    pub fn parse(s: &str) -> Option<View> {
        View::ALL.iter().copied().find(|v| v.name() == s || (s == "final" && *v == View::Final))
    }
}

/// Every knob in the capture chain. Ranges are what the UI exposes; nothing clamps them.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// RF noise, composite units (0.01 = 1 IRE rms). 0..0.08
    pub noise: f32,
    /// Per-line timing jitter in 4fsc samples (1 sample = 69.8 ns). 0..2
    pub jitter: f32,
    /// Rolling 60 Hz hum bar. 0..1
    pub hum: f32,
    /// Diagonal RF interference. 0..1
    pub beat: f32,
    /// Subcarrier phase error at the console, degrees. -30..30
    pub hue: f32,
    /// Decoder tint, degrees; cancels an equal and opposite phase error. -30..30
    pub tint: f32,
    /// Decoder saturation, 1 = unity. 0..2
    pub sat: f32,
    pub yc_mode: YcMode,
    /// Notch half-width in MHz (notch mode only). 0.6..2.5
    pub yc_mhz: f32,
    /// Decoder chroma low-pass, -3 dB in MHz. 0.4..2.0
    pub chroma_mhz: f32,
    /// Luma peaking (sharpening halo). 0..1.5
    pub peak: f32,
    pub levels: Levels,
    /// Run the MPEG-2 8×8 DCT quantisation stage.
    pub mpeg: bool,
    /// MPEG-2 quantiser_scale. 2 (pristine) .. 62 (awful)
    pub q: f32,
    /// How much q rises across a 15-frame GOP. 0..1
    pub pump: f32,
    /// Upsample 4:2:0 chroma as progressive (the chroma upsampling error bug).
    pub cue: bool,
    pub deint: Deint,
    /// Fraction cropped from each edge on display. 0..0.08
    pub overscan: f32,
    pub view: View,
}

impl Params {
    #[allow(clippy::too_many_arguments)]
    const fn of(yc_mode: YcMode, yc_mhz: f32, chroma_mhz: f32, peak: f32, noise: f32, jitter: f32, hum: f32, beat: f32,
                hue: f32, sat: f32, levels: Levels, q: f32, pump: f32, cue: bool, deint: Deint) -> Params {
        Params { noise, jitter, hum, beat, hue, tint: 0.0, sat, yc_mode, yc_mhz, chroma_mhz, peak, levels,
                 mpeg: true, q, pump, cue, deint, overscan: 0.03, view: View::Final }
    }
    /// Cheap Bt878 PCI TV card: notch filter, dot crawl, software MPEG-2.
    pub const BT878: Params = Params::of(YcMode::Notch, 1.3, 1.1, 0.7, 0.018, 0.5, 0.12, 0.15, 4.0, 1.05, Levels::Matched, 12.0, 0.6, false, Deint::Weave);
    /// Card with a hardware MPEG-2 encoder and a 3-line comb.
    pub const HARDWARE_MPEG2: Params = Params::of(YcMode::Comb3, 1.6, 1.3, 0.35, 0.01, 0.25, 0.05, 0.0, 0.0, 1.0, Levels::Matched, 8.0, 0.4, false, Deint::Weave);
    /// Bargain USB 2.0 dongle: 2-line comb, raised blacks, heavy compression, CUE bug, blended fields.
    pub const USB_DONGLE: Params = Params::of(YcMode::Comb2, 1.2, 0.8, 1.0, 0.03, 0.9, 0.2, 0.3, -6.0, 1.15, Levels::RaisedBlacks, 22.0, 0.8, true, Deint::Blend);
    /// Near-transparent reference chain.
    pub const CLEAN: Params = Params::of(YcMode::Comb3, 1.6, 1.3, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, Levels::Matched, 2.0, 0.0, false, Deint::Weave);

    pub const PRESETS: [(&'static str, Params); 4] = [
        ("bt878", Params::BT878), ("pvr", Params::HARDWARE_MPEG2), ("usb", Params::USB_DONGLE), ("clean", Params::CLEAN),
    ];
    pub fn preset(name: &str) -> Option<Params> {
        Params::PRESETS.iter().find(|(n, _)| *n == name).map(|(_, p)| p.clone())
    }
    /// Effective quantiser scale for a frame (rises across the 15-frame GOP when pumping is on).
    pub fn q_eff(&self, frame: u32) -> f32 {
        self.q * (1.0 + self.pump * 0.7 * ((frame % 15) as f32 / 14.0))
    }
}

impl Default for Params { fn default() -> Self { Params::BT878 } }

// ------------------------------------------------------------------------------------------ planes

/// A float image with `ch` channels, row 0 at the bottom.
#[derive(Clone, Debug)]
pub struct Plane { pub w: usize, pub h: usize, pub ch: usize, pub data: Vec<f32> }

impl Plane {
    pub fn new(w: usize, h: usize, ch: usize) -> Plane { Plane { w, h, ch, data: vec![0.0; w * h * ch] } }
    #[inline] pub fn at(&self, x: usize, y: usize) -> &[f32] { let i = (y * self.w + x) * self.ch; &self.data[i..i + self.ch] }
    #[inline] pub fn get(&self, x: usize, y: usize, c: usize) -> f32 { self.data[(y * self.w + x) * self.ch + c] }
    /// Rows as parallel mutable chunks.
    fn rows_mut(&mut self) -> rayon::slice::ChunksMut<'_, f32> { let w = self.w * self.ch; self.data.par_chunks_mut(w) }
}

// ------------------------------------------------------------------------------------------ math

const RGB2YIQ: [[f32; 3]; 3] = [[0.299, 0.587, 0.114], [0.596, -0.274, -0.322], [0.211, -0.523, 0.312]];
const YIQ2RGB: [[f32; 3]; 3] = [[1.0, 0.956, 0.621], [1.0, -0.272, -0.647], [1.0, -1.106, 1.703]];

#[inline] fn mul3(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
     m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
     m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2]]
}
#[inline] fn pcg(v: u32) -> u32 {
    let s = v.wrapping_mul(747796405).wrapping_add(2891336453);
    let w = ((s >> ((s >> 28).wrapping_add(4))) ^ s).wrapping_mul(277803737);
    (w >> 22) ^ w
}
#[inline] fn rnd(v: u32) -> f32 { pcg(v) as f32 * (1.0 / 4294967296.0) }
/// Subcarrier phase in quarter cycles: 227.5 cycles/line, 262.5-line field offset, ½ cycle per frame.
#[inline] fn quarter(x: i32, line: i32, frame: i32) -> usize { ((x + 2 * (line >> 1) + 3 * (line & 1) + 2 * (frame & 1)) & 3) as usize }
#[inline] fn clampi(v: i32, hi: i32) -> usize { v.clamp(0, hi) as usize }
#[inline] fn sig(mhz: f32) -> f32 { (MHZ / mhz).max(0.6) }
#[inline] fn gauss(d: f32, s: f32) -> f32 { (-d * d / (2.0 * s * s)).exp() }
#[inline] fn mix(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }

const QM: [f32; 64] = [
    8., 16., 19., 22., 26., 27., 29., 34., 16., 16., 22., 24., 27., 29., 34., 37.,
    19., 22., 26., 27., 29., 34., 34., 38., 22., 22., 26., 27., 29., 34., 37., 40.,
    22., 26., 27., 29., 32., 35., 40., 48., 26., 27., 29., 32., 35., 40., 48., 58.,
    26., 27., 29., 34., 38., 46., 56., 69., 27., 29., 35., 38., 46., 56., 69., 83.];

// ------------------------------------------------------------------------------------------ pipeline

/// Holds every intermediate stage so they can be viewed, and reuses the buffers between frames.
pub struct Pipeline {
    pub frame: u32,
    pub src: Plane,      // 720×480 RGB, woven fields
    pub comp: Plane,     // 764×480 composite signal (1 ch)
    pub yc1: Plane,      // 764×480 (S, raw C)
    pub yc2: Plane,      // 764×480 (Y, C, S)
    pub dec: Plane,      // 720×480 RGB decoded
    pub luma: Plane,     // 720×480 Y' 8-bit studio range (1 ch)
    pub chroma: Plane,   // 360×240 (Cb, Cr) 8-bit
    pub out: Plane,      // 720×480 RGB final
    scratch_y: Plane,
    scratch_c: Plane,
    dct_cos: [[f32; 8]; 8],
}

impl Default for Pipeline { fn default() -> Self { Self::new() } }

impl Pipeline {
    pub fn new() -> Pipeline {
        let mut dct_cos = [[0.0f32; 8]; 8];
        for i in 0..8 { for n in 0..8 { dct_cos[i][n] = (((2 * n + 1) * i) as f32 * PI / 16.0).cos(); } }
        Pipeline {
            frame: 0,
            src: Plane::new(CAP_W, CAP_H, 3), comp: Plane::new(COMP_W, CAP_H, 1), yc1: Plane::new(COMP_W, CAP_H, 2),
            yc2: Plane::new(COMP_W, CAP_H, 3), dec: Plane::new(CAP_W, CAP_H, 3), luma: Plane::new(CAP_W, CAP_H, 1),
            chroma: Plane::new(CAP_W / 2, CAP_H / 2, 2), out: Plane::new(CAP_W, CAP_H, 3),
            scratch_y: Plane::new(CAP_W, CAP_H, 1), scratch_c: Plane::new(CAP_W / 2, CAP_H / 2, 2), dct_cos,
        }
    }

    /// Runs one NTSC frame. `field_a` and `field_b` are 640×480 RGB8 console frames, row 0 at the bottom;
    /// `field_a` is the earlier one (even lines). Pass the same slice twice for a still image.
    pub fn process(&mut self, p: &Params, field_a: &[u8], field_b: &[u8]) {
        assert_eq!(field_a.len(), SRC_W * SRC_H * 3); assert_eq!(field_b.len(), SRC_W * SRC_H * 3);
        self.frame += 1;
        let frame = self.frame;
        self.pass_src(field_a, field_b);
        self.pass_enc(p, frame);
        self.pass_yc1(p);
        self.pass_yc2(p);
        self.pass_dem(p, frame);
        self.pass_toy();
        self.pass_toc();
        if p.mpeg {
            let q = p.q_eff(frame);
            self.mpeg2(q);
        }
        self.pass_out(p, frame);
    }

    /// 1. Weave two console frames into the 720×480 raster (704 active, 8-pixel blanked edges).
    fn pass_src(&mut self, a: &[u8], b: &[u8]) {
        self.src.rows_mut().enumerate().for_each(|(y, row)| {
            let field = if y & 1 == 0 { a } else { b };
            let v = (y as f32 + 0.5) / CAP_H as f32;
            for x in 0..CAP_W {
                let u = (x as f32 + 0.5 - 8.0) / 704.0;
                let px = &mut row[x * 3..x * 3 + 3];
                if (0.0..=1.0).contains(&u) { bilinear_rgb8(field, SRC_W, SRC_H, u, v, px); } else { px.fill(0.0); }
            }
        });
    }

    /// 2. Composite encoder: band-limit Y/I/Q, modulate, add cable impairments.
    fn pass_enc(&mut self, p: &Params, frame: u32) {
        let t = frame as f32 / 29.97;
        let rad = PI / 180.0;
        let setup = if p.levels == Levels::CrushedBlacks { 0.0 } else { 0.075 };
        let (sy, si, sq) = (MHZ / 4.2, MHZ / 1.3, MHZ / 0.5);
        const SC: f32 = 720.0 / 764.0;
        let taps: Vec<(f32, [f32; 3])> = (-12..=12).filter_map(|k| {
            let kk = k as f32; let g = [gauss(kk, sy), gauss(kk, si), gauss(kk, sq)];
            if g[2] < 1e-4 { None } else { Some((kk, g)) }
        }).collect();
        let wsum = taps.iter().fold([0.0f32; 3], |w, (_, g)| [w[0] + g[0], w[1] + g[1], w[2] + g[2]]);
        let hue = p.hue * rad;
        let ph: [(f32, f32); 4] = std::array::from_fn(|q| { let a = q as f32 * 0.5 * PI + hue; (a.cos(), a.sin()) });
        let src = &self.src;
        let (noise, jitter, hum, beat) = (p.noise, p.jitter, p.hum, p.beat);
        // Taps are one composite sample apart, so each line is resampled to YIQ once (with 12 samples of
        // margin either side) and then convolved; identical result to interpolating under every tap.
        const MARGIN: i32 = 12;
        self.comp.rows_mut().enumerate().for_each(|(line, row)| {
            let li = line as i32;
            let lseed = (line as u32).wrapping_mul(7919).wrapping_add(frame.wrapping_mul(104729));
            let jit = jitter * (0.6 * (line as f32 * 0.043 + t * 2.7).sin() + 0.8 * (rnd(lseed) - 0.5));
            let humv = hum * 0.07 * (2.0 * PI * (line as f32 / 480.0 + t * 0.06)).sin();
            let mut yiq_line = [[0.0f32; 3]; COMP_W + 2 * MARGIN as usize];
            for (i, slot) in yiq_line.iter_mut().enumerate() {
                let xs = ((i as i32 - MARGIN) as f32 + 0.5 + jit) * SC;
                *slot = mul3(&RGB2YIQ, src_at(src, xs, line));
            }
            for x in 0..COMP_W {
                let mut s = [0.0f32; 3];
                for (kk, g) in &taps {
                    let yiq = &yiq_line[(x as i32 + *kk as i32 + MARGIN) as usize];
                    s[0] += g[0] * yiq[0]; s[1] += g[1] * yiq[1]; s[2] += g[2] * yiq[2];
                }
                let yiq = [s[0] / wsum[0], s[1] / wsum[1], s[2] / wsum[2]];
                let (c, sn) = ph[quarter(x as i32, li, frame as i32)];
                let mut sigv = setup + (1.0 - setup) * (yiq[0] + yiq[1] * c + yiq[2] * sn);
                sigv += humv;
                sigv += beat * 0.035 * (2.0 * PI * (x as f32 * 0.093 + line as f32 * 0.31 + t * 1.7)).sin();
                let ns = (x as u32).wrapping_mul(73856093) ^ (line as u32).wrapping_mul(19349663) ^ frame.wrapping_mul(83492791);
                let u1 = rnd(ns).max(1e-7); let u2 = rnd(ns ^ 0x9e3779b9);
                sigv += noise * (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos();
                row[x] = sigv;
            }
        });
    }

    /// 3a. Y/C separation, stage 1: notch input or field-line comb, then a zero-DC horizontal bandpass.
    fn pass_yc1(&mut self, p: &Params) {
        let comp = &self.comp; let mode = p.yc_mode;
        let s = |x: i32, l: i32| comp.get(clampi(x, 763), clampi(l, 479), 0);
        let raw_c = |x: i32, l: i32| match mode {
            YcMode::Notch => s(x, l),
            YcMode::Comb2 => 0.5 * (s(x, l) - s(x, l + 2)),
            YcMode::Comb3 => 0.25 * (2.0 * s(x, l) - s(x, l - 2) - s(x, l + 2)),
        };
        self.yc1.rows_mut().enumerate().for_each(|(l, row)| {
            let l = l as i32;
            for x in 0..COMP_W as i32 {
                let c0 = if mode == YcMode::Notch {
                    0.25 * (2.0 * raw_c(x, l) - raw_c(x - 2, l) - raw_c(x + 2, l))
                } else {
                    raw_c(x, l) - (6.0 * raw_c(x, l) + 4.0 * (raw_c(x - 2, l) + raw_c(x + 2, l)) + raw_c(x - 4, l) + raw_c(x + 4, l)) / 16.0
                };
                let o = &mut row[x as usize * 2..x as usize * 2 + 2];
                o[0] = s(x, l); o[1] = c0;
            }
        });
    }

    /// 3b. Stage 2: gaussian-windowed fsc bandpass sets the notch width; Y = S − C.
    fn pass_yc2(&mut self, p: &Params) {
        let sig_b = if p.yc_mode == YcMode::Notch { sig(p.yc_mhz) } else { 0.0 };
        let taps: Vec<(i32, f32)> = (-12..=12).step_by(2).map(|k| {
            let g = if sig_b > 0.0 { gauss(k as f32, sig_b) } else { 0.0 };
            let sg = if ((k / 2) & 1) == 0 { 1.0 } else { -1.0 };
            (k, g * sg)
        }).collect();
        let n: f32 = taps.iter().map(|(_, gs)| gs.abs()).sum();
        let yc1 = &self.yc1;
        self.yc2.rows_mut().enumerate().for_each(|(l, row)| {
            for x in 0..COMP_W {
                let t0 = yc1.at(x, l); let s = t0[0];
                let c = if sig_b <= 0.0 { t0[1] } else {
                    let mut c = 0.0;
                    for (k, gs) in &taps { c += gs * yc1.get(clampi(x as i32 + k, 763), l, 1); }
                    c / n
                };
                let o = &mut row[x * 3..x * 3 + 3];
                o[0] = s - c; o[1] = c; o[2] = s;
            }
        });
    }

    /// 4. Quadrature demodulation, chroma low-pass, luma peaking, levels, ADC clip → 720 samples.
    fn pass_dem(&mut self, p: &Params, frame: u32) {
        let rad = PI / 180.0;
        let setup_dec = if p.levels == Levels::RaisedBlacks { 0.0 } else { 0.075 };
        let sig_d = sig(p.chroma_mhz);
        let tint = -p.tint * rad;
        let ph: [(f32, f32); 4] = std::array::from_fn(|q| { let a = q as f32 * 0.5 * PI + tint; (2.0 * a.cos(), 2.0 * a.sin()) });
        // per-x geometry is the same on every line: precompute it
        struct Col { xc: f32, n: i32, w: [f32; 25], wsum: f32 }
        let cols: Vec<Col> = (0..CAP_W).map(|x| {
            let xc = (x as f32 + 0.5) * (764.0 / 720.0) - 0.5;
            let n = (xc + 0.5).floor() as i32;
            let mut w = [0.0f32; 25]; let mut wsum = 0.0;
            for k in -12..=12 { let d = (n + k) as f32 - xc; let g = gauss(d, sig_d); w[(k + 12) as usize] = g; wsum += g; }
            Col { xc, n, w, wsum }
        }).collect();
        let yc2 = &self.yc2;
        let (peak, sat) = (p.peak, p.sat);
        let kk = 1.0 / (1.0 - setup_dec);
        self.dec.rows_mut().enumerate().for_each(|(l, row)| {
            let li = l as i32;
            let yat = |xc: f32| { let f = xc.floor(); let i = f as i32; let t = xc - f;
                mix(yc2.get(clampi(i, 763), l, 0), yc2.get(clampi(i + 1, 763), l, 0), t) };
            for (x, col) in cols.iter().enumerate() {
                let mut y = yat(col.xc);
                y += peak * (y - 0.5 * (yat(col.xc - 1.7) + yat(col.xc + 1.7)));
                let (mut iv, mut qv) = (0.0f32, 0.0f32);
                for k in -12..=12i32 {
                    let xi = clampi(col.n + k, 763);
                    let g = col.w[(k + 12) as usize];
                    let c = yc2.get(xi, l, 1);
                    let (cs, sn) = ph[quarter(xi as i32, li, frame as i32)];
                    iv += g * c * cs; qv += g * c * sn;
                }
                iv /= col.wsum; qv /= col.wsum;
                let rgb = mul3(&YIQ2RGB, [(y - setup_dec) * kk, iv * kk * sat, qv * kk * sat]);
                let o = &mut row[x * 3..x * 3 + 3];
                for c in 0..3 { o[c] = rgb[c].clamp(0.0, 1.0); }
            }
        });
    }

    /// 5a. BT.601 luma plane, studio range, 8-bit.
    fn pass_toy(&mut self) {
        let dec = &self.dec;
        self.luma.rows_mut().enumerate().for_each(|(l, row)| {
            for x in 0..CAP_W {
                let c = dec.at(x, l);
                let y = (16.0 + 219.0 * (0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2])) / 255.0;
                row[x] = (y * 255.0 + 0.5).floor() / 255.0;
            }
        });
    }

    /// 5b. 4:2:0 chroma plane (360×240), field-based subsampling as an interlaced MPEG-2 encoder does.
    fn pass_toc(&mut self) {
        let dec = &self.dec;
        self.chroma.rows_mut().enumerate().for_each(|(cr, row)| {
            let p = cr & 1; let j = cr >> 1; let r0 = 4 * j + p; let r1 = r0 + 2;
            for cx in 0..CAP_W / 2 {
                let (a, b, c, d) = (dec.at(2 * cx, r0), dec.at(2 * cx + 1, r0), dec.at(2 * cx, r1), dec.at(2 * cx + 1, r1));
                let m = [0.25 * (a[0] + b[0] + c[0] + d[0]), 0.25 * (a[1] + b[1] + c[1] + d[1]), 0.25 * (a[2] + b[2] + c[2] + d[2])];
                let y = 0.299 * m[0] + 0.587 * m[1] + 0.114 * m[2];
                let cb = (m[2] - y) * 0.564; let crv = (m[0] - y) * 0.713;
                row[cx * 2] = (128.0 + 224.0 * cb + 0.5).floor() / 255.0;
                row[cx * 2 + 1] = (128.0 + 224.0 * crv + 0.5).floor() / 255.0;
            }
        });
    }

    /// 6. MPEG-2 intra coding of both planes: forward DCT (H, then V with quantisation), inverse DCT (V, H).
    fn mpeg2(&mut self, q: f32) {
        let cos = self.dct_cos;
        dct_pass(&self.luma, &mut self.scratch_y, &cos, 0, false, None);
        dct_pass(&self.scratch_y, &mut self.luma, &cos, 1, false, Some(q));
        dct_pass(&self.luma, &mut self.scratch_y, &cos, 1, true, None);
        dct_pass(&self.scratch_y, &mut self.luma, &cos, 0, true, None);
        dct_pass(&self.chroma, &mut self.scratch_c, &cos, 0, false, None);
        dct_pass(&self.scratch_c, &mut self.chroma, &cos, 1, false, Some(q));
        dct_pass(&self.chroma, &mut self.scratch_c, &cos, 1, true, None);
        dct_pass(&self.scratch_c, &mut self.chroma, &cos, 0, true, None);
    }

    /// 7. 4:2:0 upsampling (field-correct, or the progressive CUE bug), YCbCr → RGB, deinterlace.
    fn pass_out(&mut self, p: &Params, frame: u32) {
        let (luma, chroma) = (&self.luma, &self.chroma);
        let cue = p.cue; let deint = p.deint;
        let cc = |cx: i32, cr: i32| { let s = chroma.at(clampi(cx, 359), clampi(cr, 239)); (s[0], s[1]) };
        let chroma_row = |cxf: f32, cr: i32| { let f = cxf.floor(); let i = f as i32; let t = cxf - f;
            let (a, b) = (cc(i, cr), cc(i + 1, cr)); (mix(a.0, b.0, t), mix(a.1, b.1, t)) };
        let pix = |x: usize, l: i32| -> [f32; 3] {
            let l = l.clamp(0, 479);
            let y = luma.get(x, l as usize, 0);
            let cxf = (x as f32 - 0.5) * 0.5;
            let (cb, cr) = if cue {
                let r = (l as f32 - 0.5) * 0.5; let f = r.floor(); let fi = f as i32;
                let (a, b) = (chroma_row(cxf, fi), chroma_row(cxf, fi + 1)); (mix(a.0, b.0, r - f), mix(a.1, b.1, r - f))
            } else {
                let p = l & 1; let m = l >> 1;
                let r = (m as f32 - 0.5) * 0.5; let f = r.floor(); let j = f as i32;
                let (a, b) = (chroma_row(cxf, 2 * j + p), chroma_row(cxf, 2 * (j + 1) + p)); (mix(a.0, b.0, r - f), mix(a.1, b.1, r - f))
            };
            let yf = (y * 255.0 - 16.0) / 219.0; let cb = (cb * 255.0 - 128.0) / 224.0; let cr = (cr * 255.0 - 128.0) / 224.0;
            [yf + 1.402 * cr, yf - 0.344136 * cb - 0.714136 * cr, yf + 1.772 * cb]
        };
        self.out.rows_mut().enumerate().for_each(|(l, row)| {
            let li = l as i32;
            for x in 0..CAP_W {
                let c = match deint {
                    Deint::Bob => { let p = (frame & 1) as i32; if (li & 1) == p { pix(x, li) } else { avg(pix(x, li - 1), pix(x, li + 1)) } }
                    Deint::Blend => avg(pix(x, li), pix(x, li ^ 1)),
                    Deint::Weave => pix(x, li),
                };
                let o = &mut row[x * 3..x * 3 + 3];
                for k in 0..3 { o[k] = c[k].clamp(0.0, 1.0); }
            }
        });
    }

    /// The plane a view shows, and how to tone-map it for display.
    pub fn stage(&self, v: View) -> (&Plane, u8) {
        match v {
            View::Source => (&self.src, 0), View::Composite => (&self.comp, 1), View::Luma => (&self.yc2, 2),
            View::Chroma => (&self.yc2, 3), View::Decoded => (&self.dec, 0), View::Final => (&self.out, 0),
        }
    }
}

#[inline] fn avg(a: [f32; 3], b: [f32; 3]) -> [f32; 3] { [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), 0.5 * (a[2] + b[2])] }

/// Linear sample of the woven source along a line, matching the shader's `srcAt`.
#[inline] fn src_at(src: &Plane, xs: f32, line: usize) -> [f32; 3] {
    let f = xs - 0.5; let fi = f.floor(); let i0 = fi as i32; let t = f - fi;
    let a = src.at(clampi(i0, 719), line); let b = src.at(clampi(i0 + 1, 719), line);
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

/// GL_LINEAR + CLAMP_TO_EDGE sample of an RGB8 texture at normalised (u, v).
#[inline] fn bilinear_rgb8(img: &[u8], w: usize, h: usize, u: f32, v: f32, out: &mut [f32]) {
    let fx = u * w as f32 - 0.5; let fy = v * h as f32 - 0.5;
    let (x0f, y0f) = (fx.floor(), fy.floor()); let (tx, ty) = (fx - x0f, fy - y0f);
    let x0 = clampi(x0f as i32, w as i32 - 1); let x1 = clampi(x0f as i32 + 1, w as i32 - 1);
    let y0 = clampi(y0f as i32, h as i32 - 1); let y1 = clampi(y0f as i32 + 1, h as i32 - 1);
    for c in 0..3 {
        let p = |x: usize, y: usize| img[(y * w + x) * 3 + c] as f32 / 255.0;
        out[c] = mix(mix(p(x0, y0), p(x1, y0), tx), mix(p(x0, y1), p(x1, y1), tx), ty);
    }
}

/// One separable 8-point DCT pass. `dir` 0 = horizontal, 1 = vertical. `quant` applies MPEG-2 intra
/// quantisation with the default matrix (only meaningful on the second forward pass).
fn dct_pass(input: &Plane, out: &mut Plane, cos: &[[f32; 8]; 8], dir: u8, inverse: bool, quant: Option<f32>) {
    let (w, ch) = (input.w, input.ch);
    let cf = |k: usize| if k == 0 { 0.35355339f32 } else { 0.5 };
    out.rows_mut().enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let pos = if dir == 0 { x } else { y }; let i = pos & 7; let base = pos - i;
            let mut acc = [0.0f32; 4];
            for n in 0..8 {
                let (qx, qy) = if dir == 0 { (base + n, y) } else { (x, base + n) };
                let v = input.at(qx, qy);
                let coef = if !inverse { cf(i) * cos[i][n] } else { cf(n) * cos[n][i] };
                for c in 0..ch { acc[c] += v[c] * coef; }
            }
            if let Some(q) = quant {
                let (u, vv) = (x & 7, y & 7);
                for c in 0..ch {
                    let f = acc[c] * 255.0;
                    let f = if u == 0 && vv == 0 { (f / 8.0 + 0.5).floor() * 8.0 } else {
                        let st = QM[vv * 8 + u] * q / 16.0;
                        f.signum() * ((f.abs() / st + 0.4).floor()).max(0.0) * st
                    };
                    acc[c] = f / 255.0;
                }
            }
            row[x * ch..x * ch + ch].copy_from_slice(&acc[..ch]);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(rgb: [u8; 3]) -> Vec<u8> { (0..SRC_W * SRC_H).flat_map(|_| rgb).collect() }

    #[test]
    fn clean_round_trip_is_transparent() {
        let mut pl = Pipeline::new();
        let mut p = Params::CLEAN; p.mpeg = false;
        for col in [[191u8, 191, 191], [191, 0, 0], [0, 191, 191], [128, 64, 32]] {
            let img = flat(col);
            pl.process(&p, &img, &img); pl.process(&p, &img, &img);
            let o = pl.out.at(360, 240);
            for c in 0..3 { assert!((o[c] * 255.0 - col[c] as f32).abs() < 4.0, "{col:?} -> {o:?}"); }
        }
    }

    #[test]
    fn phase_error_cancelled_by_tint() {
        let mut pl = Pipeline::new(); let img = flat([191, 0, 0]);
        let mut p = Params::CLEAN; p.mpeg = false; p.hue = 20.0;
        pl.process(&p, &img, &img); let bad = pl.out.at(360, 240).to_vec();
        p.tint = -20.0; pl.process(&p, &img, &img); let fixed = pl.out.at(360, 240);
        let err = |o: &[f32]| (o[0] * 255.0 - 191.0).abs().max((o[1] * 255.0).abs()).max((o[2] * 255.0).abs());
        assert!(err(&bad) > 20.0 && err(fixed) < 4.0, "bad {} fixed {}", err(&bad), err(fixed));
    }

    #[test]
    fn mpeg_blocking_grows_with_q() {
        let mut img = vec![0u8; SRC_W * SRC_H * 3];
        for y in 0..SRC_H { for x in 0..SRC_W { let i = (y * SRC_W + x) * 3;
            img[i] = (x / 3) as u8; img[i + 1] = ((x * 7 + y * 3) % 251) as u8; img[i + 2] = (y / 2) as u8; } }
        let block = |q: f32| { let mut pl = Pipeline::new(); let mut p = Params::CLEAN; p.q = q; pl.process(&p, &img, &img);
            let (mut on, mut off) = (0.0, 0.0);
            for y in 16..464 { for x in 16..704 { let d = (pl.luma.get(x, y, 0) - pl.luma.get(x - 1, y, 0)).abs();
                if x % 8 == 0 { on += d } else { off += d / 7.0 } } }
            on / off };
        let (a, b) = (block(2.0), block(40.0));
        assert!(b > 1.4 * a, "q2 {a} q40 {b}");
    }
}
