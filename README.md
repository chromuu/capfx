---

# capfx: composite capture '05

Plays video or images through a simulation of a 2005-era composite capture card and saves the result
as PNG, MP4, WebM or GIF. Pure Rust on the CPU (no GPU needed), with ffmpeg doing the decoding and encoding.

The chain: console frame → interlaced 720×480 raster → NTSC composite at 4fsc → notch or comb Y/C separation →
quadrature demodulation → BT.601 4:2:0 → MPEG-2 intra quantisation → interlaced playback. It is a line-for-line
port of the WebGL shader version; the two agree to within 1/255 outside MPEG quantiser rounding ties (see Testing).

## Requirements

- Rust 1.80 or newer (`rustup` from rust-lang.org)
- `ffmpeg` and `ffprobe` on your PATH (any recent build; used for all media in and out)
- For the GUI and `play`: a desktop. Linux needs X11 or Wayland; macOS and Windows work as they are.

## Build

```
cargo build --release
```

The binary is `target/release/capfx` (`capfx.exe` on Windows).

## Use

```
capfx                                           # the graphical interface (double-click capfx.exe does the same)
capfx gameplay.mp4                              # GUI with a file already open
capfx play   gameplay.mp4                       # bare player window with keyboard controls
capfx play   screenshot.png --preset usb        # a still image: the noise, crawl and GOP still animate
capfx render gameplay.mp4 -o captured.mp4       # process the whole file, audio copied across
capfx render gameplay.mp4 -o clip.gif --start 12 --duration 3 --size 480x360
capfx still  screenshot.png -o captured.png --preset pvr --q 20
capfx presets                                   # the four card presets
```

Any format ffmpeg can read works as input. Output format follows the extension: `.mp4`/`.mkv`/`.mov`
(H.264, 4:3 display aspect like a real capture), `.webm` (VP9), `.gif`.

### The GUI

Open a file with the button, by pasting a path, or by dropping it on the window. The right panel has every
setting as a slider or toggle with the four card presets at the top; changes apply live, and while paused the
current frame is re-processed so you can tune on a still. The View row shows any pipeline stage (the raw
composite signal, separated luma or chroma, the decoded picture before MPEG-2) and the A/B wipe compares
against the clean source. Transport: play/pause, single-frame step, loop, and a seek bar.

Saving: **Snapshot PNG** writes what's on screen at 720×480; **Record MP4** records what's on screen in real
time until you stop it; **Export whole file** re-processes the file from the start (or from a start time, for
a duration) at a chosen size and quality with the source audio, in the background with a progress bar. The
same keys as the bare player work here: `space`, `.`, `S`, `R`, and `O` to open.

### Bare player keys (`capfx play`)

| Key | Does |
|---|---|
| `space` / `.` | pause / step one frame |
| `1` `2` `3` `4` | Bt878 PCI tuner / hardware MPEG-2 card / USB 2.0 dongle / clean reference |
| `v` | cycle the view: source, composite signal, luma, chroma, decoded, capture |
| `w` then `←` `→` | A/B wipe against the clean source |
| `n` / `d` / `m` | Y/C separation mode / deinterlacer / MPEG-2 on-off |
| `[` `]` | quantiser scale down / up |
| `-` `=` | RF noise down / up |
| `s` | save a PNG of what's on screen (720×480) |
| `r` | start / stop recording an MP4 of what's on screen, in real time |
| `q` / `esc` | quit |

Files are saved in the current directory as `capfx_<date>_<time>.png` / `.mp4`.

### Options

Every setting is a flag, and flags override the preset whatever the order:

```
--preset <bt878|pvr|usb|clean>
--noise <0..0.08>   --jitter <0..2>   --hum <0..1>   --beat <0..1>
--hue <deg>  --tint <deg>  --sat <0..2>  --peak <0..1.5>
--yc <notch|comb2|comb3>  --notch-mhz <0.6..2.5>  --chroma-mhz <0.4..2>
--levels <matched|raised|crushed>
--no-mpeg  --q <2..62>  --pump <0..1>  --cue
--deint <weave|bob|blend>
--overscan <0..0.08>          edge crop (default 0.03)
--view <source|composite|luma|chroma|decoded|capture>
--size <WxH>                  output size (default 720x480; the file is flagged 4:3)
--start <s> --duration <s> --frames <n>
--crf <n>                     encoder quality for render (default 18)
--no-audio                    don't copy the source audio
--loop                        play: repeat the file
```

Example: a bad composite cable into a Bt878 card, saved at 640×480:

```
capfx render tape.mkv -o bad_cable.mp4 --preset bt878 --noise 0.04 --jitter 1.2 --hum 0.4 --size 640x480
```

## Fields and frame rate

Output is always 29.97 fps interlaced. Sources at 50 fps or above supply one picture per field, so motion
combs like a real capture; slower sources use consecutive frames as the two fields. Still images repeat.

## Speed

The filter is multithreaded (rayon) and processes at the real capture size, 720×480, whatever the input or
output size. On the 2-core machine used to test it, that was 13 frames per second; it scales with cores, so a
desktop with 8 or more should run at or near real time. `render` is never limited by this, and recordings
from the player stay in real time by repeating frames if the machine falls behind.

## Using the filter in your own program

```rust
use capfx::{Pipeline, Params};
let mut pl = Pipeline::new();
let p = Params::BT878;              // or HARDWARE_MPEG2, USB_DONGLE, CLEAN; every field is public
// field_a, field_b: 640×480 RGB8 console frames, bottom row first (earlier frame first)
pl.process(&p, &field_a, &field_b);
let rgb = capfx::present::render(&pl, p.view, p.overscan, None, 720, 480); // RGB8, top row first
```

`pl.out`, `pl.dec`, `pl.comp` and the other stages are available as float planes.

