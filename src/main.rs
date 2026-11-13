//! capfx: play media through a 2005-era composite capture card simulation and save the result.

use capfx::ffmpeg::{self, Encoder};
use capfx::job::{self, Fields, RenderOpts};
use capfx::{present, Deint, Levels, Params, Pipeline, View, YcMode, CAP_H, CAP_W};
use std::time::{Duration, Instant};

const USAGE: &str = "\
capfx: 2005-era composite capture card simulation

USAGE
  capfx [gui] [<media>]                           open the graphical interface (default with no arguments)
  capfx play   <media>  [options]                 bare player window with keyboard controls
  capfx render <media>  -o out.mp4 [options]      process a whole file headlessly (mp4, mkv, webm, gif)
  capfx still  <image>  -o out.png [options]      process one picture
  capfx presets                                   list the card presets

OPTIONS (all commands)
  --preset <bt878|pvr|usb|clean>   card preset (default bt878)
  --noise <0..0.08>  --jitter <0..2>  --hum <0..1>  --beat <0..1>
  --hue <deg>  --tint <deg>  --sat <0..2>  --peak <0..1.5>
  --yc <notch|comb2|comb3>  --notch-mhz <0.6..2.5>  --chroma-mhz <0.4..2>
  --levels <matched|raised|crushed>
  --no-mpeg  --q <2..62>  --pump <0..1>  --cue
  --deint <weave|bob|blend>
  --overscan <0..0.08>                 edge crop on output (default 0.03)
  --view <source|composite|luma|chroma|decoded|capture>
  --size <WxH>                         output picture size (default 720x480, shown 4:3)
  --start <seconds>  --duration <seconds>
  --frames <n>                         stop after n output frames (render/still: still defaults to 1)
  --frame <n>                          still: NTSC frame number to render (changes crawl phase, GOP position)
  --crf <n>                            encoder quality for render (default 18)
  --no-audio                           render: don't copy the source audio
  --loop                               play: repeat the file

PLAYER KEYS
  space pause   . step   1-4 presets   v view   w A/B wipe   n Y/C mode   d deinterlace   m MPEG on/off
  [ ] quantiser   - = noise   s save PNG   r start/stop recording MP4   q/esc quit
";

struct Opts {
    cmd: String, input: Option<String>, output: Option<String>, params: Params,
    size: (usize, usize), start: f64, duration: Option<f64>, frames: Option<u64>, frame: Option<u32>,
    crf: u32, audio: bool, loop_input: bool,
}

fn parse_args() -> Result<Opts, String> {
    let mut o_input_first: Option<String> = None;
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "gui".to_string());
    let cmd = if cmd != "gui" && !["play", "render", "still", "presets", "-h", "--help"].contains(&cmd.as_str()) && !cmd.starts_with('-') {
        // `capfx some_file.mp4` (or a file dropped on the exe): open it in the GUI
        o_input_first = Some(cmd); "gui".to_string()
    } else { cmd };
    let mut o = Opts { cmd, input: o_input_first, output: None, params: Params::BT878, size: (CAP_W, CAP_H), start: 0.0,
                       duration: None, frames: None, frame: None, crf: 18, audio: true, loop_input: false };
    let mut rest: Vec<String> = args.collect();
    // preset first, so other flags override it whatever the order
    if let Some(i) = rest.iter().position(|a| a == "--preset") {
        let name = rest.get(i + 1).cloned().ok_or("--preset needs a name")?;
        o.params = Params::preset(&name).ok_or_else(|| format!("unknown preset '{name}' (bt878, pvr, usb, clean)"))?;
        rest.drain(i..i + 2);
    }
    let mut it = rest.into_iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or(format!("{a} needs a value"));
        let f = |s: String| s.parse::<f32>().map_err(|_| format!("bad number '{s}'"));
        let p = &mut o.params;
        match a.as_str() {
            "-o" | "--output" => o.output = Some(val()?),
            "--noise" => p.noise = f(val()?)?, "--jitter" => p.jitter = f(val()?)?, "--hum" => p.hum = f(val()?)?,
            "--beat" => p.beat = f(val()?)?, "--hue" => p.hue = f(val()?)?, "--tint" => p.tint = f(val()?)?,
            "--sat" => p.sat = f(val()?)?, "--peak" => p.peak = f(val()?)?, "--notch-mhz" => p.yc_mhz = f(val()?)?,
            "--chroma-mhz" => p.chroma_mhz = f(val()?)?, "--q" => p.q = f(val()?)?, "--pump" => p.pump = f(val()?)?,
            "--overscan" => p.overscan = f(val()?)?,
            "--yc" => p.yc_mode = match val()?.as_str() { "notch" => YcMode::Notch, "comb2" => YcMode::Comb2, "comb3" => YcMode::Comb3, s => return Err(format!("bad --yc '{s}'")) },
            "--levels" => p.levels = match val()?.as_str() { "matched" => Levels::Matched, "raised" => Levels::RaisedBlacks, "crushed" => Levels::CrushedBlacks, s => return Err(format!("bad --levels '{s}'")) },
            "--deint" => p.deint = match val()?.as_str() { "weave" => Deint::Weave, "bob" => Deint::Bob, "blend" => Deint::Blend, s => return Err(format!("bad --deint '{s}'")) },
            "--view" => { let s = val()?; p.view = View::parse(&s).ok_or(format!("bad --view '{s}'"))?; }
            "--no-mpeg" => p.mpeg = false, "--cue" => p.cue = true, "--no-audio" => o.audio = false, "--loop" => o.loop_input = true,
            "--size" => { let s = val()?; let (w, h) = s.split_once('x').ok_or("--size wants WxH")?;
                          o.size = (w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?); }
            "--start" => o.start = val()?.parse().map_err(|_| "bad --start")?,
            "--duration" => o.duration = Some(val()?.parse().map_err(|_| "bad --duration")?),
            "--frames" => o.frames = Some(val()?.parse().map_err(|_| "bad --frames")?),
            "--frame" => o.frame = Some(val()?.parse().map_err(|_| "bad --frame")?),
            "--crf" => o.crf = val()?.parse().map_err(|_| "bad --crf")?,
            "-h" | "--help" => return Err(USAGE.to_string()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n\n{USAGE}")),
            _ => { if o.input.is_none() { o.input = Some(a) } else { return Err(format!("unexpected argument {a}")) } }
        }
    }
    Ok(o)
}

fn main() {
    let o = match parse_args() { Ok(o) => o, Err(e) => { eprintln!("{e}"); std::process::exit(2); } };
    if o.cmd == "presets" {
        for (name, p) in Params::PRESETS.iter() {
            println!("{name:6}  yc={:?} notch={}MHz chroma={}MHz peak={} noise={} jitter={} q={} pump={} cue={} deint={:?}",
                     p.yc_mode, p.yc_mhz, p.chroma_mhz, p.peak, p.noise, p.jitter, p.q, p.pump, p.cue, p.deint);
        }
        return;
    }
    let result = match o.cmd.as_str() {
        "gui" => capfx::gui::run(o.input.clone()),
        "play" => play(o), "render" => render(o), "still" => still(o),
        _ => { eprintln!("{USAGE}"); std::process::exit(2); }
    };
    if let Err(e) = result { eprintln!("error: {e}"); std::process::exit(1); }
}

// ------------------------------------------------------------------ helpers

fn open_input(o: &Opts) -> Result<(String, ffmpeg::Probe), String> {
    ffmpeg::check_tools().map_err(|e| e.to_string())?;
    let input = o.input.clone().ok_or("no input file given")?;
    let probe = ffmpeg::probe(&input).map_err(|e| e.to_string())?;
    Ok((input, probe))
}

fn frame_limit(o: &Opts) -> Option<u64> {
    match (o.frames, o.duration) { (Some(n), _) => Some(n), (None, Some(d)) => Some((d * 29.97).round() as u64), _ => None }
}

// ------------------------------------------------------------------ render

fn render(o: Opts) -> Result<(), String> {
    let (input, _) = open_input(&o)?;
    let out = o.output.clone().ok_or("render needs -o <file>")?;
    let opts = RenderOpts { input, output: out.clone(), params: o.params.clone(), size: o.size, start: o.start, frames: frame_limit(&o), crf: o.crf, audio: o.audio };
    let t0 = Instant::now();
    let written = job::render_file(&opts, &mut |n, total| {
        if n % 30 == 0 { match total { Some(t) => eprint!("\r{n} / {t} frames ({:.1} fps)   ", n as f64 / t0.elapsed().as_secs_f64()), None => eprint!("\r{n} frames ({:.1} fps)   ", n as f64 / t0.elapsed().as_secs_f64()) } }
        true
    }).map_err(|e| e.to_string())?;
    eprintln!("\rwrote {written} frames to {out} in {:.1}s", t0.elapsed().as_secs_f64());
    Ok(())
}

// ------------------------------------------------------------------ still

fn still(o: Opts) -> Result<(), String> {
    let (input, _) = open_input(&o)?;
    let out = o.output.clone().ok_or("still needs -o <file.png>")?;
    let img = ffmpeg::load_image(&input).map_err(|e| e.to_string())?;
    let mut pl = Pipeline::new();
    // run up to the requested frame number so crawl phase and GOP position match the web version's goto()
    let target = o.frame.unwrap_or(1).max(1);
    pl.frame = target - 1;
    pl.process(&o.params, &img, &img);
    let (w, h) = o.size;
    let rgb = present::render(&pl, o.params.view, o.params.overscan, None, w, h);
    if out.to_ascii_lowercase().ends_with(".f32") {
        // raw float dump of the stage at native size, top row first: used by the parity test
        let (stage, _) = pl.stage(o.params.view);
        let mut bytes = Vec::with_capacity(stage.w * stage.h * stage.ch * 4);
        for y in (0..stage.h).rev() { for x in 0..stage.w { for c in 0..stage.ch { bytes.extend_from_slice(&stage.get(x, y, c).to_le_bytes()); } } }
        std::fs::write(&out, bytes).map_err(|e| e.to_string())?;
    } else {
        ffmpeg::save_png(&out, w, h, &rgb).map_err(|e| e.to_string())?;
    }
    eprintln!("wrote {out}");
    Ok(())
}

// ------------------------------------------------------------------ play

fn play(mut o: Opts) -> Result<(), String> {
    use minifb::{Key, KeyRepeat, Scale, ScaleMode, Window, WindowOptions};
    let (input, probe) = open_input(&o)?;
    let is_image = job::is_image(&input, &probe);
    let still_img = if is_image { Some(ffmpeg::load_image(&input).map_err(|e| e.to_string())?) } else { None };
    let mut fields = if is_image { None } else { Some(Fields::open(&input, &probe, o.start, o.loop_input).map_err(|e| e.to_string())?) };

    let (ww, wh) = (960usize, 720usize);
    let mut win = Window::new("capfx: composite capture '05", ww, wh,
        WindowOptions { resize: true, scale: Scale::X1, scale_mode: ScaleMode::Stretch, ..WindowOptions::default() })
        .map_err(|e| format!("couldn't open a window: {e}"))?;
    win.set_target_fps(60);

    let mut pl = Pipeline::new();
    let mut paused = false;
    let mut wipe: Option<f32> = None;
    let mut rec: Option<(Encoder, String, Instant)> = None; // encoder, file, wall-clock start
    let mut rgb = vec![0u8; ww * wh * 3];
    let mut buf: Vec<u32> = Vec::with_capacity(ww * wh);
    let frame_dt = Duration::from_secs_f64(1.0 / 29.97);
    let mut next_tick = Instant::now();
    let mut ended = false;
    let preset_names = ["Bt878 PCI tuner", "Hardware MPEG-2", "USB 2.0 dongle", "Clean reference"];
    let mut status = String::from(preset_names[0]);
    let mut dirty = true;

    let step = |pl: &mut Pipeline, fields: &mut Option<Fields>, params: &Params| -> Result<bool, String> {
        match (fields, &still_img) {
            (Some(f), _) => { if !f.advance().map_err(|e| e.to_string())? { return Ok(false); } pl.process(params, &f.prev, &f.cur); }
            (None, Some(img)) => pl.process(params, img, img),
            _ => unreachable!(),
        }
        Ok(true)
    };

    while win.is_open() && !win.is_key_down(Key::Escape) && !win.is_key_down(Key::Q) {
        let p = &mut o.params;
        let mut changed = false;
        for k in win.get_keys_pressed(KeyRepeat::No) {
            changed = true;
            match k {
                Key::Space => paused = !paused,
                Key::Period => { paused = true; if !ended { ended = !step(&mut pl, &mut fields, p)?; } dirty = true; }
                Key::Key1 | Key::Key2 | Key::Key3 | Key::Key4 => {
                    let i = match k { Key::Key1 => 0, Key::Key2 => 1, Key::Key3 => 2, _ => 3 };
                    let keep = (p.view, p.overscan); *p = Params::PRESETS[i].1.clone(); p.view = keep.0; p.overscan = keep.1;
                    status = preset_names[i].to_string();
                }
                Key::V => { let i = View::ALL.iter().position(|v| *v == p.view).unwrap_or(5); p.view = View::ALL[(i + 1) % 6]; status = format!("view: {}", p.view.name()); }
                Key::W => { wipe = if wipe.is_none() { Some(0.5) } else { None }; status = if wipe.is_some() { "A/B wipe: source left, capture right (←/→ to move)".into() } else { "wipe off".into() }; }
                Key::Left => if let Some(s) = wipe.as_mut() { *s = (*s - 0.05).max(0.0) },
                Key::Right => if let Some(s) = wipe.as_mut() { *s = (*s + 0.05).min(1.0) },
                Key::N => { p.yc_mode = match p.yc_mode { YcMode::Notch => YcMode::Comb2, YcMode::Comb2 => YcMode::Comb3, YcMode::Comb3 => YcMode::Notch }; status = format!("Y/C: {:?}", p.yc_mode); }
                Key::D => { p.deint = match p.deint { Deint::Weave => Deint::Bob, Deint::Bob => Deint::Blend, Deint::Blend => Deint::Weave }; status = format!("deinterlace: {:?}", p.deint); }
                Key::M => { p.mpeg = !p.mpeg; status = format!("MPEG-2: {}", if p.mpeg { "on" } else { "off" }); }
                Key::LeftBracket => { p.q = (p.q - 2.0).max(2.0); status = format!("q {}", p.q); }
                Key::RightBracket => { p.q = (p.q + 2.0).min(62.0); status = format!("q {}", p.q); }
                Key::Minus => { p.noise = (p.noise - 0.005).max(0.0); status = format!("noise {:.1} IRE", p.noise * 100.0); }
                Key::Equal => { p.noise = (p.noise + 0.005).min(0.08); status = format!("noise {:.1} IRE", p.noise * 100.0); }
                Key::S => {
                    let name = job::stamped("png");
                    let shot = present::render(&pl, p.view, p.overscan, wipe, CAP_W, CAP_H);
                    ffmpeg::save_png(&name, CAP_W, CAP_H, &shot).map_err(|e| e.to_string())?;
                    status = format!("saved {name}");
                }
                Key::R => {
                    if let Some((enc, name, _)) = rec.take() {
                        let n = enc.finish().map_err(|e| e.to_string())?; status = format!("saved {name} ({n} frames, {:.1} s)", n as f64 / 29.97);
                    } else {
                        let name = job::stamped("mp4");
                        rec = Some((Encoder::open(&name, CAP_W, CAP_H, 29.97, None, o.crf).map_err(|e| e.to_string())?, name.clone(), Instant::now()));
                        status = format!("recording {name}");
                    }
                }
                _ => changed = false,
            }
        }
        if changed { dirty = true; eprintln!("{status}"); }

        let now = Instant::now();
        if !paused && now >= next_tick && !ended {
            ended = !step(&mut pl, &mut fields, p)?;
            if ended { status = "end of file".into(); }
            next_tick += frame_dt;
            if now > next_tick + frame_dt * 4 { next_tick = now; } // we fell behind; don't try to catch up
            if let Some((enc, _, started)) = rec.as_mut() {
                // Keep the file in real time: if processing can't keep up with 29.97 Hz, repeat the frame.
                let due = (started.elapsed().as_secs_f64() * 29.97).round() as u64;
                let frame_rgb = present::render(&pl, p.view, p.overscan, None, CAP_W, CAP_H);
                let mut writes = due.saturating_sub(enc.frames).clamp(1, 8);
                while writes > 0 { enc.write(&frame_rgb).map_err(|e| e.to_string())?; writes -= 1; }
            }
            dirty = true;
        } else if paused || ended { next_tick = now; }

        if dirty && pl.frame > 0 {
            // The buffer is always exactly the window size, letterboxed here, so minifb never rescales it.
            let (cw, ch) = win.get_size();
            let (cw, ch) = (cw.max(64), ch.max(48));
            let (pw, ph) = fit_4_3(cw, ch);
            rgb = present::render(&pl, p.view, p.overscan, wipe, pw, ph);
            letterbox(&rgb, pw, ph, cw, ch, &mut buf);
            win.update_with_buffer(&buf, cw, ch).map_err(|e| e.to_string())?;
            let rec_tag = if rec.is_some() { "  ● REC" } else { "" };
            win.set_title(&format!("capfx  {}  frame {}  q {:.1}{}{}  |  {}", if paused { "⏸" } else { "▶" }, pl.frame, p.q_eff(pl.frame), if p.mpeg { "" } else { " (MPEG off)" }, rec_tag, status));
            dirty = false;
        } else {
            win.update();
        }
        if paused { std::thread::sleep(Duration::from_millis(8)); }
    }
    if let Some((enc, name, _)) = rec.take() { let n = enc.finish().map_err(|e| e.to_string())?; eprintln!("saved {name} ({n} frames)"); }
    let _ = rgb;
    Ok(())
}

/// Centres a pw×ph RGB8 picture in a cw×ch 0RGB window buffer with black bars.
fn letterbox(rgb: &[u8], pw: usize, ph: usize, cw: usize, ch: usize, out: &mut Vec<u32>) {
    out.clear(); out.resize(cw * ch, 0);
    let (x0, y0) = ((cw - pw) / 2, (ch - ph) / 2);
    for y in 0..ph {
        let src = &rgb[y * pw * 3..(y + 1) * pw * 3];
        let dst = &mut out[(y0 + y) * cw + x0..(y0 + y) * cw + x0 + pw];
        for (d, s) in dst.iter_mut().zip(src.chunks_exact(3)) { *d = (s[0] as u32) << 16 | (s[1] as u32) << 8 | s[2] as u32; }
    }
}

fn fit_4_3(w: usize, h: usize) -> (usize, usize) {
    if w * 3 > h * 4 { (h * 4 / 3, h) } else { (w, w * 3 / 4) }
}
