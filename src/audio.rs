//! Audio playback for the player: ffmpeg decodes the source's audio to interleaved f32 PCM, a reader thread
//! fills a short buffer, and a cpal output stream plays it. The number of sample frames actually played is the
//! clock the video follows, so picture and sound stay together even when the filter can't keep up.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Linear gain shared between the interface and the playing stream (f32 bits); 0 is silent.
#[derive(Clone)]
pub struct Volume(Arc<AtomicU32>);

impl Volume {
    pub fn new(v: f32) -> Volume { Volume(Arc::new(AtomicU32::new(v.to_bits()))) }
    pub fn set(&self, v: f32) { self.0.store(v.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed) }
    pub fn get(&self) -> f32 { f32::from_bits(self.0.load(Ordering::Relaxed)) }
}

struct Buf { q: Mutex<VecDeque<f32>>, cv: Condvar, eof: AtomicBool, stop: AtomicBool, played: AtomicU64 }

/// One playing audio stream, from a start time. Drop it to stop.
pub struct Audio { _stream: cpal::Stream, child: Child, buf: Arc<Buf>, rate: u32 }

impl Audio {
    pub fn open(path: &str, start: f64, loop_input: bool, volume: Volume) -> Result<Audio, String> {
        let device = cpal::default_host().default_output_device().ok_or("no audio output device")?;
        let cfg = device.default_output_config().map_err(|e| e.to_string())?;
        let (rate, channels) = (cfg.sample_rate().0, cfg.channels() as usize);

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "fatal", "-nostdin"]);
        if loop_input { cmd.args(["-stream_loop", "-1"]); }
        if start > 0.0 { cmd.args(["-ss", &format!("{start}")]); }
        cmd.args(["-i", path, "-vn", "-sn", "-map", "0:a:0", "-f", "f32le", "-ac", &channels.to_string(), "-ar", &rate.to_string(), "pipe:1"]);
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
        let mut out = child.stdout.take().unwrap();

        let buf = Arc::new(Buf { q: Mutex::new(VecDeque::new()), cv: Condvar::new(), eof: AtomicBool::new(false), stop: AtomicBool::new(false), played: AtomicU64::new(0) });
        let cap = rate as usize * channels / 2; // half a second ahead
        let b = buf.clone();
        std::thread::Builder::new().name("capfx-audio".into()).spawn(move || {
            let mut raw = vec![0u8; 4096 * 4];
            loop {
                let n = match out.read(&mut raw) { Ok(0) | Err(_) => break, Ok(n) => n - n % 4 };
                let mut q = b.q.lock().unwrap();
                while q.len() > cap && !b.stop.load(Ordering::Relaxed) { q = b.cv.wait(q).unwrap(); }
                if b.stop.load(Ordering::Relaxed) { break; }
                q.extend(raw[..n].chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
            }
            b.eof.store(true, Ordering::Relaxed);
        }).map_err(|e| e.to_string())?;

        let sc: cpal::StreamConfig = cfg.clone().into();
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &sc, channels, buf.clone(), volume),
            cpal::SampleFormat::I16 => build::<i16>(&device, &sc, channels, buf.clone(), volume),
            cpal::SampleFormat::U16 => build::<u16>(&device, &sc, channels, buf.clone(), volume),
            cpal::SampleFormat::I32 => build::<i32>(&device, &sc, channels, buf.clone(), volume),
            f => Err(format!("unsupported audio sample format {f:?}")),
        };
        let stream = match stream { Ok(s) => s, Err(e) => { let _ = child.kill(); let _ = child.wait(); return Err(e); } };
        stream.play().map_err(|e| e.to_string())?;
        Ok(Audio { _stream: stream, child, buf, rate })
    }

    /// Seconds of audio that have actually reached the device since this stream started.
    pub fn played(&self) -> f64 { self.buf.played.load(Ordering::Relaxed) as f64 / self.rate as f64 }

    /// True once the source has run out of audio and everything has been played.
    pub fn finished(&self) -> bool { self.buf.eof.load(Ordering::Relaxed) && self.buf.q.lock().unwrap().is_empty() }
}

fn build<T: SizedSample + FromSample<f32>>(device: &cpal::Device, sc: &cpal::StreamConfig, channels: usize, buf: Arc<Buf>, volume: Volume) -> Result<cpal::Stream, String> {
    device.build_output_stream(sc, move |data: &mut [T], _| {
        let gain = volume.get();
        let mut q = buf.q.lock().unwrap();
        let avail = (q.len() / channels * channels).min(data.len());
        for (d, s) in data[..avail].iter_mut().zip(q.drain(..avail)) { *d = T::from_sample((s * gain).clamp(-1.0, 1.0)); }
        drop(q);
        for d in &mut data[avail..] { *d = T::from_sample(0.0f32); } // underrun: silence, and the clock doesn't move
        buf.played.fetch_add((avail / channels) as u64, Ordering::Relaxed);
        buf.cv.notify_one();
    }, |e| eprintln!("audio: {e}"), None).map_err(|e| e.to_string())
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.buf.stop.store(true, Ordering::Relaxed);
        self.buf.cv.notify_all();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
