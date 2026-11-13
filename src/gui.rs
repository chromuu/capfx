//! Desktop GUI: a player with every control on screen, snapshots, real-time recording and whole-file export.
//! The filter runs on a worker thread so the interface stays responsive on slow machines.

use crate::audio::{Audio, Volume};
use crate::ffmpeg::{self, Encoder};
use crate::job::{self, Fields, FpsMeter, RenderOpts};
use crate::{present, Deint, Levels, Params, Pipeline, View, YcMode, CAP_H, CAP_W};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ worker

enum Cmd {
    Open(String), Params(Params), Playing(bool), Step, Loop(bool), Seek(f64), Scrub(f64), Wipe(Option<f32>),
    Snapshot(String), Record(Option<String>),
}

#[derive(Default)]
struct Shared {
    frame: Option<Vec<u8>>,     // 720×480 RGB8, top row first, freshly rendered
    frame_no: u32,
    fps: f32,
    media: Option<MediaInfo>,
    position: f64,              // seconds into the file
    ended: bool,
    recording: Option<(String, f64)>,
    messages: Vec<(Instant, String)>,
}

#[derive(Clone)]
struct MediaInfo { path: String, name: String, width: u32, height: u32, fps: f64, duration: Option<f64>, image: bool, has_audio: bool }

struct Worker {
    rx: Receiver<Cmd>, shared: Arc<Mutex<Shared>>, ctx: egui::Context,
    params: Params, pl: Pipeline, playing: bool, loop_input: bool, wipe: Option<f32>,
    media: Option<MediaInfo>, fields: Option<Fields>, still: Option<Vec<u8>>, start: f64,
    rec: Option<(Encoder, String, Instant)>, meter: FpsMeter, next_tick: Instant,
    /// Playing sound and the frame number it started at; while it plays it is the clock the picture follows.
    audio: Option<(Audio, u32)>, volume: Volume, audio_failed: bool,
}

impl Worker {
    fn say(&self, msg: impl Into<String>) { self.shared.lock().unwrap().messages.push((Instant::now(), msg.into())); }

    fn open(&mut self, path: String, start: f64) {
        self.stop_recording();
        self.audio = None;
        self.fields = None; self.still = None; self.media = None;
        let probe = match ffmpeg::check_tools().and_then(|_| ffmpeg::probe(&path)) { Ok(p) => p, Err(e) => { self.say(format!("Can't open: {e}")); return; } };
        let image = job::is_image(&path, &probe);
        let name = std::path::Path::new(&path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        if image {
            match ffmpeg::load_image(&path) { Ok(img) => self.still = Some(img), Err(e) => { self.say(format!("Can't open: {e}")); return; } }
        } else {
            match Fields::open(&path, &probe, start, self.loop_input) { Ok(f) => self.fields = Some(f), Err(e) => { self.say(format!("Can't open: {e}")); return; } }
        }
        let info = MediaInfo { path: path.clone(), name, width: probe.width, height: probe.height, fps: probe.fps, duration: probe.duration, image, has_audio: probe.has_audio && !image };
        self.media = Some(info.clone());
        self.start = start;
        self.pl = Pipeline::new();
        let mut s = self.shared.lock().unwrap();
        s.media = Some(info); s.ended = false; s.position = start; s.frame_no = 0;
        drop(s);
        self.playing = true;
        self.next_tick = Instant::now();
        self.step();
    }

    /// One NTSC frame; false at end of file.
    fn step(&mut self) -> bool {
        let ok = match (&mut self.fields, &self.still) {
            (Some(f), _) => match f.advance() { Ok(true) => { self.pl.process(&self.params, &f.prev, &f.cur); true } Ok(false) => false, Err(e) => { self.say(format!("Decode error: {e}")); false } },
            (None, Some(img)) => { self.pl.process(&self.params, img, img); true }
            _ => false,
        };
        if !ok { self.playing = false; let mut s = self.shared.lock().unwrap(); s.ended = true; return false; }
        self.meter.tick();
        if let Some((enc, _, started)) = self.rec.as_mut() {
            // Keep the file in real time: if the machine can't keep up with 29.97 Hz, repeat the frame.
            let due = (started.elapsed().as_secs_f64() * 29.97).round() as u64;
            let rgb = present::render(&self.pl, self.params.view, self.params.overscan, None, CAP_W, CAP_H);
            let mut writes = due.saturating_sub(enc.frames).clamp(1, 8);
            while writes > 0 { if let Err(e) = enc.write(&rgb) { self.say(format!("Recording failed: {e}")); self.rec = None; break; } writes -= 1; }
        }
        self.publish();
        true
    }

    fn publish(&mut self) {
        if self.pl.frame == 0 { return; }
        let rgb = present::render(&self.pl, self.params.view, self.params.overscan, self.wipe, CAP_W, CAP_H);
        let mut s = self.shared.lock().unwrap();
        s.frame = Some(rgb); s.frame_no = self.pl.frame; s.fps = self.meter.fps;
        s.position = self.start + self.pl.frame as f64 / 29.97;
        if self.loop_input { if let Some(d) = self.media.as_ref().and_then(|m| m.duration).filter(|d| *d > 0.0) { s.position %= d; } }
        s.recording = self.rec.as_ref().map(|(e, n, _)| (n.clone(), e.frames as f64 / 29.97));
        drop(s);
        self.ctx.request_repaint();
    }

    fn stop_recording(&mut self) {
        if let Some((enc, name, _)) = self.rec.take() {
            match enc.finish() { Ok(n) => self.say(format!("Saved {name} ({:.1} s)", n as f64 / 29.97)), Err(e) => self.say(format!("Recording failed: {e}")) }
            self.publish();
        }
    }

    fn run(mut self) {
        let frame_dt = Duration::from_secs_f64(1.0 / 29.97);
        loop {
            // drain commands
            loop {
                match self.rx.try_recv() {
                    Ok(cmd) => match cmd {
                        Cmd::Open(p) => self.open(p, 0.0),
                        Cmd::Params(p) => { let reprocess = !self.playing; self.params = p; if reprocess && self.pl.frame > 0 { self.reprocess(); } }
                        Cmd::Playing(b) => { if !b { self.audio = None; } if b && self.shared.lock().unwrap().ended { if let Some(m) = self.media.clone() { self.open(m.path, 0.0); } } self.playing = b; self.next_tick = Instant::now(); }
                        Cmd::Step => { self.playing = false; self.audio = None; self.step(); }
                        Cmd::Loop(b) => { self.loop_input = b; if let Some(m) = self.media.clone() { if !m.image { let pos = self.shared.lock().unwrap().position; self.open(m.path, pos); } } }
                        Cmd::Seek(t) => { if let Some(m) = self.media.clone() { if !m.image { let was = self.playing; self.open(m.path, t); self.playing = was; } } }
                        Cmd::Scrub(t) => { if let Some(m) = self.media.clone() { if !m.image { self.open(m.path, t); self.playing = false; } } }
                        Cmd::Wipe(w) => { self.wipe = w; self.publish(); }
                        Cmd::Snapshot(path) => {
                            let rgb = present::render(&self.pl, self.params.view, self.params.overscan, self.wipe, CAP_W, CAP_H);
                            match ffmpeg::save_png(&path, CAP_W, CAP_H, &rgb) { Ok(()) => self.say(format!("Saved {path}")), Err(e) => self.say(format!("Save failed: {e}")) }
                        }
                        Cmd::Record(Some(path)) => {
                            self.stop_recording();
                            match Encoder::open(&path, CAP_W, CAP_H, 29.97, None, 18) { Ok(e) => { self.rec = Some((e, path.clone(), Instant::now())); self.say(format!("Recording to {path}")); self.publish(); } Err(e) => self.say(format!("Can't record: {e}")) }
                        }
                        Cmd::Record(None) => self.stop_recording(),
                    },
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => { self.stop_recording(); return; }
                }
            }
            if self.playing && self.media.is_some() {
                self.ensure_audio();
                if let Some((a, base)) = &self.audio {
                    if a.finished() { self.audio = None; self.next_tick = Instant::now(); continue; }
                    // Audio is the clock: catch up by skipping frames through the filter, wait when ahead.
                    let due = *base + (a.played() * 29.97) as u32;
                    if self.pl.frame >= due { std::thread::sleep(Duration::from_millis(3)); continue; }
                    let mut skip = (due - self.pl.frame - 1).min(15);
                    while skip > 0 {
                        match self.fields.as_mut().map(|f| f.advance()) { Some(Ok(true)) => self.pl.frame += 1, _ => break }
                        skip -= 1;
                    }
                    self.step();
                    continue;
                }
                let now = Instant::now();
                if now >= self.next_tick {
                    self.step();
                    self.next_tick += frame_dt;
                    if now > self.next_tick + frame_dt * 4 { self.next_tick = now; }
                } else {
                    std::thread::sleep((self.next_tick - now).min(Duration::from_millis(4)));
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// Starts sound at the current position if the file has some and it isn't already playing.
    fn ensure_audio(&mut self) {
        if self.audio.is_some() || self.audio_failed || self.fields.is_none() { return; }
        let Some(m) = self.media.clone().filter(|m| m.has_audio) else { return };
        let pos = self.shared.lock().unwrap().position;
        match Audio::open(&m.path, pos, self.loop_input, self.volume.clone()) {
            Ok(a) => self.audio = Some((a, self.pl.frame)),
            Err(e) => { self.audio_failed = true; self.say(format!("No sound: {e}")); }
        }
    }

    /// Re-run the current frame with new settings while paused (the frame counter stays put).
    fn reprocess(&mut self) {
        let n = self.pl.frame;
        if let Some(img) = self.still.clone() { self.pl.frame = n - 1; self.pl.process(&self.params, &img, &img); }
        else if let Some(f) = &self.fields { let (a, b) = (f.prev.clone(), f.cur.clone()); self.pl.frame = n - 1; self.pl.process(&self.params, &a, &b); }
        self.publish();
    }
}

// ------------------------------------------------------------------ export job

struct Export { progress: Arc<Mutex<(u64, Option<u64>, Option<Result<u64, String>>)>>, cancel: Arc<Mutex<bool>>, output: String }

fn start_export(opts: RenderOpts, ctx: egui::Context) -> Export {
    let progress = Arc::new(Mutex::new((0u64, None, None)));
    let cancel = Arc::new(Mutex::new(false));
    let (p2, c2, output) = (progress.clone(), cancel.clone(), opts.output.clone());
    std::thread::spawn(move || {
        let mut last = Instant::now();
        let r = job::render_file(&opts, &mut |done, total| {
            if last.elapsed() > Duration::from_millis(100) { let mut p = p2.lock().unwrap(); p.0 = done; p.1 = total; last = Instant::now(); ctx.request_repaint(); }
            !*c2.lock().unwrap()
        });
        p2.lock().unwrap().2 = Some(r.map_err(|e| e.to_string()));
        ctx.request_repaint();
    });
    Export { progress, cancel, output }
}

// ------------------------------------------------------------------ app

pub struct App {
    tx: Sender<Cmd>, shared: Arc<Mutex<Shared>>,
    params: Params, preset: Option<usize>, playing: bool, loop_input: bool, wipe: bool, split: f32,
    tex: Option<egui::TextureHandle>, last_frame_no: u32,
    path_box: String, seek: f64, seeking: bool,
    export: Option<Export>, export_out: String, export_start: String, export_dur: String, export_size: usize, export_crf: u32, export_audio: bool,
    toasts: Vec<(Instant, String)>,
    volume: Volume, vol: f32, muted: bool, last_scrub: Instant,
}

const PRESET_NAMES: [&str; 4] = ["Bt878 PCI tuner", "Hardware MPEG-2", "USB 2.0 dongle", "Clean reference"];
const SIZES: [(&str, (usize, usize)); 4] = [("720×480 (DVD)", (720, 480)), ("640×480", (640, 480)), ("960×720", (960, 720)), ("1440×1080", (1440, 1080))];

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<String>) -> App {
        let (tx, rx) = channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let params = Params::BT878;
        let volume = Volume::new(0.8);
        let (ctx, sh, pr, vol) = (cc.egui_ctx.clone(), shared.clone(), params.clone(), volume.clone());
        // the worker is built on its own thread: the audio stream it owns can't move between threads
        std::thread::Builder::new().name("capfx-worker".into()).spawn(move || { let worker = Worker { rx, shared: sh, ctx, params: pr, pl: Pipeline::new(), playing: false,
            loop_input: true, wipe: None, media: None, fields: None, still: None, start: 0.0, rec: None, meter: FpsMeter::default(), next_tick: Instant::now(), audio: None, volume: vol, audio_failed: false }; worker.run() }).expect("worker thread");
        let mut app = App { tx, shared, params, preset: Some(0), playing: true, loop_input: true, wipe: false, split: 0.5, tex: None, last_frame_no: 0,
            path_box: String::new(), seek: 0.0, seeking: false, export: None, export_out: String::new(), export_start: "0".into(), export_dur: String::new(),
            export_size: 0, export_crf: 18, export_audio: true, toasts: Vec::new(), volume, vol: 0.8, muted: false, last_scrub: Instant::now() };
        let _ = app.tx.send(Cmd::Loop(true));
        if let Some(p) = initial { app.open(p); }
        app
    }

    fn open(&mut self, path: String) {
        self.path_box = path.clone();
        let stem = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "capture".into());
        self.export_out = format!("{stem}_captured.mp4");
        self.playing = true;
        let _ = self.tx.send(Cmd::Open(path));
    }

    fn send_params(&mut self) { let _ = self.tx.send(Cmd::Params(self.params.clone())); }

    fn toast(&mut self, s: impl Into<String>) { self.toasts.push((Instant::now(), s.into())); }

    fn apply_volume(&self) { self.volume.set(if self.muted { 0.0 } else { self.vol }); }

    /// Seek while dragging: shows the frame under the cursor, throttled since every seek restarts the decoder.
    fn scrub(&mut self, t: f64) {
        self.seek = t;
        if self.last_scrub.elapsed() > Duration::from_millis(150) { self.last_scrub = Instant::now(); let _ = self.tx.send(Cmd::Scrub(t)); }
    }

    /// Final seek when a drag or click ends; playback resumes if it was playing.
    fn seek_to(&mut self, t: f64) {
        self.seek = t; self.seeking = false;
        let _ = self.tx.send(Cmd::Seek(t));
        let _ = self.tx.send(Cmd::Playing(self.playing));
    }

    fn pick_file(&mut self) {
        let f = rfd::FileDialog::new()
            .add_filter("Video and images", &["mp4", "mkv", "mov", "avi", "webm", "m4v", "mpg", "mpeg", "ts", "wmv", "flv", "gif", "png", "jpg", "jpeg", "bmp", "webp"])
            .add_filter("All files", &["*"]).pick_file();
        if let Some(p) = f { self.open(p.to_string_lossy().into_owned()); }
    }
}

impl App {
    /// A seek bar drawn over the bottom of the picture: shown while the pointer is over the picture or when paused;
    /// hover for the time under the cursor, click or drag to seek.
    fn timeline_overlay(&mut self, ui: &mut egui::Ui, pic: egui::Rect, media: Option<&MediaInfo>, position: f64) {
        let Some(dur) = media.filter(|m| !m.image).and_then(|m| m.duration).filter(|d| *d > 0.0) else { return };
        let hit = egui::Rect::from_min_max(egui::pos2(pic.left() + 16.0, pic.bottom() - 34.0), egui::pos2(pic.right() - 16.0, pic.bottom() - 6.0));
        let resp = ui.interact(hit, ui.id().with("timeline"), egui::Sense::click_and_drag());
        let over_pic = ui.rect_contains_pointer(pic);
        if !(over_pic || !self.playing || resp.dragged() || self.seeking) { return; }
        let t_at = |x: f32| (((x - hit.left()) / hit.width()).clamp(0.0, 1.0) as f64) * dur;
        if let Some(p) = resp.interact_pointer_pos() {
            if resp.drag_started() || resp.dragged() { self.seeking = true; self.scrub(t_at(p.x)); }
        }
        if resp.drag_stopped() || resp.clicked() { if let Some(p) = resp.interact_pointer_pos() { self.seek_to(t_at(p.x)); } }

        let shown = if self.seeking { self.seek } else { position };
        let active = resp.hovered() || resp.dragged();
        let h = if active { 8.0 } else { 4.0 };
        let y = hit.bottom() - 10.0;
        let track = egui::Rect::from_min_max(egui::pos2(hit.left(), y - h / 2.0), egui::pos2(hit.right(), y + h / 2.0));
        let painter = ui.painter_at(pic);
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(pic.left(), pic.bottom() - 48.0), pic.max), 0.0, egui::Color32::from_black_alpha(110));
        painter.rect_filled(track, h / 2.0, egui::Color32::from_white_alpha(70));
        let fx = track.left() + track.width() * (shown / dur).clamp(0.0, 1.0) as f32;
        painter.rect_filled(egui::Rect::from_min_max(track.min, egui::pos2(fx, track.bottom())), h / 2.0, egui::Color32::from_rgb(230, 80, 80));
        if active { painter.circle_filled(egui::pos2(fx, y), 7.0, egui::Color32::WHITE); }
        painter.text(egui::pos2(hit.left(), track.top() - 6.0), egui::Align2::LEFT_BOTTOM, format!("{} / {}", hms(shown), hms(dur)), egui::FontId::monospace(12.0), egui::Color32::WHITE);
        if let Some(hp) = resp.hover_pos().filter(|_| !resp.dragged()) {
            painter.text(egui::pos2(hp.x, track.top() - 6.0), egui::Align2::CENTER_BOTTOM, hms(t_at(hp.x)), egui::FontId::monospace(12.0), egui::Color32::YELLOW);
        }
        if active { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
        if over_pic { ui.ctx().request_repaint_after(Duration::from_millis(100)); }
    }
}

fn hms(t: f64) -> String { let t = t.max(0.0); format!("{:02}:{:02}:{:04.1}", (t / 3600.0) as u32, ((t % 3600.0) / 60.0) as u32, t % 60.0) }

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ---- pull state from the worker
        let (media, frame_no, fps, position, ended, recording) = {
            let mut s = self.shared.lock().unwrap();
            if let Some(rgb) = s.frame.take() {
                let img = egui::ColorImage::from_rgb([CAP_W, CAP_H], &rgb);
                match &mut self.tex { Some(t) => t.set(img, egui::TextureOptions::LINEAR), None => self.tex = Some(ctx.load_texture("capture", img, egui::TextureOptions::LINEAR)) }
            }
            let msgs: Vec<_> = s.messages.drain(..).collect();
            self.toasts.extend(msgs);
            (s.media.clone(), s.frame_no, s.fps, s.position, s.ended, s.recording.clone())
        };
        if ended && self.playing && !self.loop_input { self.playing = false; }
        if frame_no != self.last_frame_no { self.last_frame_no = frame_no; if !self.seeking { self.seek = position; } }
        self.toasts.retain(|(t, _)| t.elapsed() < Duration::from_secs(5));

        // ---- dropped files and shortcuts
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if let Some(p) = dropped.into_iter().next() { self.open(p.to_string_lossy().into_owned()); }
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing {
            ctx.input(|i| {
                if i.key_pressed(egui::Key::Space) { self.playing = !self.playing; let _ = self.tx.send(Cmd::Playing(self.playing)); }
                if i.key_pressed(egui::Key::Period) { self.playing = false; let _ = self.tx.send(Cmd::Step); }
                if i.key_pressed(egui::Key::S) { let _ = self.tx.send(Cmd::Snapshot(job::stamped("png"))); }
                if i.key_pressed(egui::Key::R) { let _ = self.tx.send(Cmd::Record(if recording.is_some() { None } else { Some(job::stamped("mp4")) })); }
            });
            let (left, right, mute, up, down) = ctx.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight), i.key_pressed(egui::Key::M), i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
            if let Some(d) = media.as_ref().filter(|m| !m.image).and_then(|m| m.duration) {
                if left { self.seek_to((position - 5.0).max(0.0)); }
                if right { self.seek_to((position + 5.0).min(d)); }
            }
            if mute { self.muted = !self.muted; }
            if up { self.vol = (self.vol + 0.1).min(1.0); self.muted = false; }
            if down { self.vol = (self.vol - 0.1).max(0.0); }
            if mute || up || down { self.apply_volume(); }
            if ctx.input(|i| i.key_pressed(egui::Key::O)) { self.pick_file(); }
        }

        // ---- right panel: controls
        let mut params_changed = false;
        egui::SidePanel::right("controls").min_width(300.0).default_width(330.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Open…").on_hover_text("Shortcut: O").clicked() { self.pick_file(); }
                    let r = ui.add(egui::TextEdit::singleline(&mut self.path_box).hint_text("or paste a path and press Enter").desired_width(f32::INFINITY));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.path_box.trim().is_empty() { let p = self.path_box.trim().to_string(); self.open(p); }
                });
                ui.add_space(8.0);

                ui.heading("Card preset");
                ui.horizontal_wrapped(|ui| {
                    for (i, name) in PRESET_NAMES.iter().enumerate() {
                        if ui.selectable_label(self.preset == Some(i), *name).clicked() {
                            let keep = (self.params.view, self.params.overscan);
                            self.params = Params::PRESETS[i].1.clone(); self.params.view = keep.0; self.params.overscan = keep.1;
                            self.preset = Some(i); params_changed = true;
                        }
                    }
                });
                ui.add_space(6.0);

                let p = &mut self.params;
                let mut c = false;
                egui::CollapsingHeader::new("Cable & signal").default_open(true).show(ui, |ui| {
                    c |= ui.add(egui::Slider::new(&mut p.noise, 0.0..=0.08).custom_formatter(|v, _| format!("{:.1} IRE", v * 100.0)).text("RF noise")).changed();
                    c |= ui.add(egui::Slider::new(&mut p.jitter, 0.0..=2.0).custom_formatter(|v, _| format!("{:.0} ns", v * 69.8)).text("Timing jitter")).changed();
                    c |= ui.add(egui::Slider::new(&mut p.hum, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)).text("Hum bar")).changed();
                    c |= ui.add(egui::Slider::new(&mut p.beat, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)).text("RF interference")).changed();
                    c |= ui.add(egui::Slider::new(&mut p.hue, -30.0..=30.0).suffix("°").text("Subcarrier phase error")).changed();
                });
                egui::CollapsingHeader::new("Video decoder").default_open(true).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Y/C separation");
                        for (m, n) in [(YcMode::Notch, "Notch"), (YcMode::Comb2, "2-line comb"), (YcMode::Comb3, "3-line comb")] { if ui.selectable_label(p.yc_mode == m, n).clicked() { p.yc_mode = m; c = true; } }
                    });
                    c |= ui.add_enabled(p.yc_mode == YcMode::Notch, egui::Slider::new(&mut p.yc_mhz, 0.6..=2.5).custom_formatter(|v, _| format!("±{v:.2} MHz")).text("Notch width")).changed();
                    c |= ui.add(egui::Slider::new(&mut p.chroma_mhz, 0.4..=2.0).suffix(" MHz").text("Chroma bandwidth")).changed();
                    c |= ui.add(egui::Slider::new(&mut p.peak, 0.0..=1.5).custom_formatter(|v, _| format!("+{:.0}%", v * 100.0)).text("Luma peaking")).changed();
                    c |= ui.add(egui::Slider::new(&mut p.sat, 0.0..=2.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)).text("Saturation")).changed();
                    c |= ui.add(egui::Slider::new(&mut p.tint, -30.0..=30.0).suffix("°").text("Tint")).changed();
                    ui.horizontal(|ui| {
                        ui.label("Black level");
                        for (m, n) in [(Levels::Matched, "Matched"), (Levels::RaisedBlacks, "Raised"), (Levels::CrushedBlacks, "Crushed")] { if ui.selectable_label(p.levels == m, n).clicked() { p.levels = m; c = true; } }
                    });
                });
                egui::CollapsingHeader::new("MPEG-2 encoder").default_open(true).show(ui, |ui| {
                    c |= ui.checkbox(&mut p.mpeg, "Encode MPEG-2").changed();
                    ui.add_enabled_ui(p.mpeg, |ui| {
                        c |= ui.add(egui::Slider::new(&mut p.q, 2.0..=62.0).step_by(2.0).text("Quantiser scale")).changed();
                        c |= ui.add(egui::Slider::new(&mut p.pump, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)).text("GOP pumping")).changed();
                        c |= ui.checkbox(&mut p.cue, "Progressive chroma upsampling (CUE bug)").changed();
                    });
                });
                egui::CollapsingHeader::new("Playback").default_open(true).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Deinterlace");
                        for (m, n) in [(Deint::Weave, "Weave"), (Deint::Bob, "Bob"), (Deint::Blend, "Blend")] { if ui.selectable_label(p.deint == m, n).clicked() { p.deint = m; c = true; } }
                    });
                    c |= ui.add(egui::Slider::new(&mut p.overscan, 0.0..=0.08).custom_formatter(|v, _| format!("{:.1}%", v * 100.0)).text("Overscan crop")).changed();
                    ui.horizontal_wrapped(|ui| {
                        ui.label("View");
                        for v in View::ALL { if ui.selectable_label(p.view == v, v.name()).clicked() { p.view = v; c = true; } }
                    });
                });
                if c { self.preset = None; params_changed = true; }

                ui.add_space(6.0);
                ui.heading("Save");
                ui.horizontal(|ui| {
                    if ui.button("Snapshot PNG").on_hover_text("Saves what's on screen, 720×480 (S)").clicked() {
                        let path = rfd::FileDialog::new().set_file_name(job::stamped("png")).add_filter("PNG", &["png"]).save_file();
                        if let Some(p) = path { let _ = self.tx.send(Cmd::Snapshot(p.to_string_lossy().into_owned())); }
                    }
                    match &recording {
                        Some((name, secs)) => { if ui.button(egui::RichText::new(format!("Stop recording  {:.1} s", secs)).color(egui::Color32::from_rgb(230, 80, 80))).clicked() { let _ = self.tx.send(Cmd::Record(None)); } ui.small(name.rsplit(['/', '\\']).next().unwrap_or(name)); }
                        None => if ui.button("Record MP4").on_hover_text("Records what's on screen (R)").clicked() {
                            let path = rfd::FileDialog::new().set_file_name(job::stamped("mp4")).add_filter("MP4", &["mp4"]).save_file();
                            if let Some(p) = path { let _ = self.tx.send(Cmd::Record(Some(p.to_string_lossy().into_owned()))); }
                        },
                    }
                });

                ui.add_space(6.0);
                ui.heading("Export whole file");
                ui.add_enabled_ui(media.is_some() && self.export.is_none(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Output");
                        ui.add(egui::TextEdit::singleline(&mut self.export_out).desired_width(170.0));
                        if ui.button("…").clicked() {
                            let path = rfd::FileDialog::new().set_file_name(&self.export_out).add_filter("Video", &["mp4", "mkv", "mov", "webm", "gif"]).save_file();
                            if let Some(p) = path { self.export_out = p.to_string_lossy().into_owned(); }
                        }
                    });
                    ui.small("Format follows the extension: mp4, mkv, mov, webm or gif");
                    ui.horizontal(|ui| {
                        ui.label("Start (s)"); ui.add(egui::TextEdit::singleline(&mut self.export_start).desired_width(50.0));
                        ui.label("Duration (s)"); ui.add(egui::TextEdit::singleline(&mut self.export_dur).hint_text("all").desired_width(50.0));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Size");
                        egui::ComboBox::from_id_salt("size").selected_text(SIZES[self.export_size].0).show_ui(ui, |ui| { for (i, (n, _)) in SIZES.iter().enumerate() { ui.selectable_value(&mut self.export_size, i, *n); } });
                        ui.label("Quality (CRF)"); ui.add(egui::DragValue::new(&mut self.export_crf).range(10..=35));
                        ui.checkbox(&mut self.export_audio, "Audio");
                    });
                    if ui.add_sized([ui.available_width(), 28.0], egui::Button::new("Export")).clicked() {
                        if let Some(m) = &media {
                            let start: f64 = self.export_start.trim().parse().unwrap_or(0.0);
                            let frames = self.export_dur.trim().parse::<f64>().ok().map(|d| (d * 29.97).round() as u64);
                            let opts = RenderOpts { input: m.path.clone(), output: self.export_out.clone(), params: self.params.clone(), size: SIZES[self.export_size].1, start, frames, crf: self.export_crf, audio: self.export_audio };
                            self.export = Some(start_export(opts, ctx.clone()));
                        }
                    }
                });
                if let Some(ex) = &self.export {
                    let (done, total, result) = { let p = ex.progress.lock().unwrap(); (p.0, p.1, p.2.clone()) };
                    match result {
                        None => {
                            let frac = total.map(|t| done as f32 / t.max(1) as f32).unwrap_or(0.0);
                            ui.add(egui::ProgressBar::new(frac).text(match total { Some(t) => format!("{done} / {t} frames"), None => format!("{done} frames") }).animate(true));
                            if ui.button("Cancel").clicked() { *ex.cancel.lock().unwrap() = true; }
                        }
                        Some(Ok(0)) => { self.toast("Export cancelled"); self.export = None; }
                        Some(Ok(n)) => { let msg = format!("Exported {} ({:.1} s)", ex.output, n as f64 / 29.97); self.export = None; self.toast(msg); }
                        Some(Err(e)) => { let msg = format!("Export failed: {e}"); self.export = None; self.toast(msg); }
                    }
                }
                ui.add_space(10.0);
                ui.small("Keys: space play/pause · . step · ←/→ seek 5 s · M mute · ↑/↓ volume · S snapshot · R record · O open");
            });
        });
        if params_changed { self.send_params(); }

        // ---- bottom: transport
        egui::TopBottomPanel::bottom("transport").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let has = media.is_some();
                if ui.add_enabled(has, egui::Button::new(if self.playing && !ended { "⏸" } else { "▶" }).min_size([32.0, 24.0].into())).clicked() {
                    self.playing = !self.playing; let _ = self.tx.send(Cmd::Playing(self.playing));
                }
                if ui.add_enabled(has, egui::Button::new("⏭").min_size([32.0, 24.0].into())).on_hover_text("Step one frame").clicked() { self.playing = false; let _ = self.tx.send(Cmd::Step); }
                if ui.checkbox(&mut self.loop_input, "Loop").changed() { let _ = self.tx.send(Cmd::Loop(self.loop_input)); }
                let seekable = media.as_ref().is_some_and(|m| !m.image && m.duration.is_some());
                let dur = media.as_ref().and_then(|m| m.duration).unwrap_or(0.0);
                ui.label(egui::RichText::new(hms(position)).monospace());
                let r = ui.add_enabled(seekable, egui::Slider::new(&mut self.seek, 0.0..=dur.max(0.1)).show_value(false));
                if r.drag_started() { self.seeking = true; }
                if r.dragged() && r.changed() { let t = self.seek; self.scrub(t); }
                if r.drag_stopped() { let t = self.seek; self.seek_to(t); }
                else if r.changed() && !r.dragged() { let t = self.seek; self.seek_to(t); }
                if seekable { ui.label(egui::RichText::new(hms(dur)).monospace()); }
                ui.separator();
                let has_audio = media.as_ref().is_some_and(|m| m.has_audio);
                let icon = if self.muted || self.vol == 0.0 { "🔇" } else if self.vol < 0.5 { "🔉" } else { "🔊" };
                if ui.add_enabled(has_audio, egui::Button::new(icon).min_size([28.0, 24.0].into())).on_hover_text("Mute (M)").clicked() { self.muted = !self.muted; self.apply_volume(); }
                if ui.add_enabled(has_audio, egui::Slider::new(&mut self.vol, 0.0..=1.0).show_value(false)).on_hover_text(format!("Volume {:.0}%  (↑/↓)", self.vol * 100.0)).changed() { self.muted = false; self.apply_volume(); }
                ui.separator();
                if ui.checkbox(&mut self.wipe, "A/B wipe").changed() { let _ = self.tx.send(Cmd::Wipe(if self.wipe { Some(self.split) } else { None })); }
                if self.wipe && ui.add(egui::Slider::new(&mut self.split, 0.0..=1.0).show_value(false)).changed() { let _ = self.tx.send(Cmd::Wipe(Some(self.split))); }
            });
            ui.horizontal(|ui| {
                let status = match &media {
                    Some(m) => format!("{}  ·  {}×{} {}  ·  frame {}  ·  {:.1} fps{}{}", m.name, m.width, m.height, if m.image { "still".to_string() } else { format!("@ {:.2} fps", m.fps) }, frame_no, fps,
                        if self.params.mpeg { format!("  ·  q {:.1}", self.params.q_eff(frame_no)) } else { "  ·  MPEG off".into() }, if recording.is_some() { "  ·  REC" } else { "" }),
                    None => "Open a video or image to start.".to_string(),
                };
                ui.small(status);
            });
            ui.add_space(2.0);
        });

        // ---- centre: picture
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::BLACK)).show(ctx, |ui| {
            let avail = ui.available_size();
            match &self.tex {
                Some(t) => {
                    let img = egui::Image::from_texture(egui::load::SizedTexture::new(t.id(), egui::vec2(640.0, 480.0))).maintain_aspect_ratio(true).fit_to_exact_size(avail);
                    let pic = ui.centered_and_justified(|ui| ui.add(img)).inner.rect;
                    self.timeline_overlay(ui, pic, media.as_ref(), position);
                }
                None => { ui.centered_and_justified(|ui| ui.label(egui::RichText::new("Open a video or image, or drop one here").color(egui::Color32::GRAY).size(18.0))); }
            }
            // toasts
            let mut y = 12.0;
            for (_, msg) in self.toasts.iter().rev().take(4) {
                let rect = ui.max_rect();
                egui::Area::new(egui::Id::new(("toast", y as i32))).fixed_pos(rect.left_top() + egui::vec2(12.0, y)).show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(msg));
                });
                y += 34.0;
            }
        });

        if self.playing && media.is_some() { ctx.request_repaint_after(Duration::from_millis(33)); }
    }
}

/// Opens the window. `initial` is an optional file to load.
pub fn run(initial: Option<String>) -> Result<(), String> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1240.0, 760.0]).with_min_inner_size([900.0, 560.0]).with_title("capfx: composite capture '05").with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("capfx", opts, Box::new(move |cc| Ok(Box::new(App::new(cc, initial))))).map_err(|e| e.to_string())
}
