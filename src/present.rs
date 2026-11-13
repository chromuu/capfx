//! Turns a pipeline stage into displayable pixels: overscan crop, 4:3 resampling, tone-mapping of the
//! signal views, and the A/B wipe. Mirrors the shader's present pass.

use crate::{Pipeline, Plane, View};
use rayon::prelude::*;

/// Converts a float plane to RGB8, top row first, at its native size. `kind` is the view's tone map.
pub fn plane_to_rgb8(p: &Plane, kind: u8) -> Vec<u8> {
    let mut out = vec![0u8; p.w * p.h * 3];
    out.par_chunks_mut(p.w * 3).enumerate().for_each(|(row, dst)| {
        let y = p.h - 1 - row;
        for x in 0..p.w { let c = tone(p.at(x, y), kind); dst[x * 3..x * 3 + 3].copy_from_slice(&c); }
    });
    out
}

#[inline]
fn tone(v: &[f32], kind: u8) -> [u8; 3] {
    let q = |f: f32| (f.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    match kind {
        1 => { let g = q(v[0] / 1.3); [g, g, g] }
        2 => { let g = q(v[0]); [g, g, g] }
        3 => { let g = q(0.5 + v[1] * 1.4); [g, g, g] }
        _ => [q(v[0]), q(v[1]), q(v[2])],
    }
}

/// Renders the chosen view into a `w`×`h` RGB8 buffer (top row first) with overscan cropping,
/// bilinear resampling and an optional wipe (`split` in 0..1 shows the clean source left of it).
pub fn render(pl: &Pipeline, view: View, overscan: f32, split: Option<f32>, w: usize, h: usize) -> Vec<u8> {
    let (stage, kind) = pl.stage(view);
    let reference = &pl.src;
    let mut out = vec![0u8; w * h * 3];
    out.par_chunks_mut(w * 3).enumerate().for_each(|(row, dst)| {
        let v0 = 1.0 - (row as f32 + 0.5) / h as f32;
        let v = overscan + (1.0 - 2.0 * overscan) * v0;
        for x in 0..w {
            let u0 = (x as f32 + 0.5) / w as f32;
            let u = overscan + (1.0 - 2.0 * overscan) * u0;
            let px = match split {
                Some(s) if (x as f32 - s * w as f32).abs() < 1.5 => [227, 161, 66],
                Some(s) if (x as f32) < s * w as f32 => tone(&bilinear(reference, u, v), 0),
                _ => tone(&bilinear(stage, u, v), kind),
            };
            dst[x * 3..x * 3 + 3].copy_from_slice(&px);
        }
    });
    out
}

fn bilinear(p: &Plane, u: f32, v: f32) -> [f32; 4] {
    let fx = u * p.w as f32 - 0.5; let fy = v * p.h as f32 - 0.5;
    let (x0f, y0f) = (fx.floor(), fy.floor()); let (tx, ty) = (fx - x0f, fy - y0f);
    let cl = |i: i32, hi: usize| i.clamp(0, hi as i32 - 1) as usize;
    let (x0, x1) = (cl(x0f as i32, p.w), cl(x0f as i32 + 1, p.w));
    let (y0, y1) = (cl(y0f as i32, p.h), cl(y0f as i32 + 1, p.h));
    let mut o = [0.0f32; 4];
    for c in 0..p.ch.min(4) {
        let a = p.get(x0, y0, c) + (p.get(x1, y0, c) - p.get(x0, y0, c)) * tx;
        let b = p.get(x0, y1, c) + (p.get(x1, y1, c) - p.get(x0, y1, c)) * tx;
        o[c] = a + (b - a) * ty;
    }
    o
}

/// Packs RGB8 into the 0RGB u32 pixels a window wants.
pub fn pack_0rgb(rgb: &[u8], out: &mut Vec<u32>) {
    out.clear();
    out.extend(rgb.chunks_exact(3).map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32));
}
