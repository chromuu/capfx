package com.example.capturefx;

/**
 * Every knob in the capture chain. Plain fields so a settings screen can bind to them.
 * Change them on the UI thread with {@link CaptureCardRenderer#setParams(CaptureParams)}.
 */
public final class CaptureParams {
    // Y/C separation in the decoder chip
    public static final int YC_NOTCH = 0, YC_COMB_2LINE = 1, YC_COMB_3LINE = 2;
    // NTSC 7.5 IRE setup handling
    public static final int LEVELS_MATCHED = 0, LEVELS_RAISED_BLACKS = 1, LEVELS_CRUSHED_BLACKS = 2;
    // Playback deinterlacing
    public static final int DEINT_WEAVE = 0, DEINT_BOB = 1, DEINT_BLEND = 2;
    // Which pipeline stage to display (handy while tuning)
    public static final int VIEW_SOURCE = 0, VIEW_COMPOSITE = 1, VIEW_LUMA = 2, VIEW_CHROMA = 3, VIEW_DECODED = 4, VIEW_FINAL = 5;

    /** RF noise, composite units (0.01 = 1 IRE rms). 0..0.08 */
    public float noise;
    /** Per-line timing jitter in 4fsc samples (1 sample = 69.8 ns). 0..2 */
    public float jitter;
    /** Rolling 60 Hz hum bar strength. 0..1 */
    public float hum;
    /** Diagonal RF interference strength. 0..1 */
    public float beat;
    /** Subcarrier phase error at the console's encoder, degrees. -30..30 */
    public float hue;
    /** Decoder tint, degrees (cancels an equal and opposite phase error). -30..30 */
    public float tint;
    /** Decoder saturation, 1 = unity. 0..2 */
    public float sat;
    /** One of YC_NOTCH, YC_COMB_2LINE, YC_COMB_3LINE. */
    public int ycMode;
    /** Notch half-width in MHz (notch mode only). 0.6..2.5 */
    public float ycMHz;
    /** Decoder chroma low-pass, -3 dB point in MHz. 0.4..2.0 */
    public float chromaMHz;
    /** Luma peaking (sharpening halo). 0..1.5 */
    public float peak;
    /** One of LEVELS_MATCHED, LEVELS_RAISED_BLACKS, LEVELS_CRUSHED_BLACKS. */
    public int levels;
    /** Run the MPEG-2 8x8 DCT quantisation stage. */
    public boolean mpeg;
    /** MPEG-2 quantiser_scale. 2 (pristine) .. 62 (awful) */
    public float q;
    /** How much q rises across a 15-frame GOP. 0..1 */
    public float pump;
    /** Upsample 4:2:0 chroma as progressive (the chroma upsampling error bug). */
    public boolean cue;
    /** One of DEINT_WEAVE, DEINT_BOB, DEINT_BLEND. */
    public int deint;
    /** Fraction cropped from each edge on display. 0..0.08 */
    public float overscan = 0.03f;
    /** One of the VIEW_ constants. */
    public int view = VIEW_FINAL;

    public CaptureParams copy() {
        CaptureParams p = new CaptureParams();
        p.set(this);
        return p;
    }

    public void set(CaptureParams o) {
        noise = o.noise; jitter = o.jitter; hum = o.hum; beat = o.beat; hue = o.hue; tint = o.tint; sat = o.sat;
        ycMode = o.ycMode; ycMHz = o.ycMHz; chromaMHz = o.chromaMHz; peak = o.peak; levels = o.levels;
        mpeg = o.mpeg; q = o.q; pump = o.pump; cue = o.cue; deint = o.deint; overscan = o.overscan; view = o.view;
    }

    private static CaptureParams of(int ycMode, float ycMHz, float chromaMHz, float peak, float noise, float jitter,
                                    float hum, float beat, float hue, float sat, int levels, float q, float pump,
                                    boolean cue, int deint) {
        CaptureParams p = new CaptureParams();
        p.ycMode = ycMode; p.ycMHz = ycMHz; p.chromaMHz = chromaMHz; p.peak = peak; p.noise = noise;
        p.jitter = jitter; p.hum = hum; p.beat = beat; p.hue = hue; p.tint = 0f; p.sat = sat; p.levels = levels;
        p.mpeg = true; p.q = q; p.pump = pump; p.cue = cue; p.deint = deint;
        return p;
    }

    /** Cheap Bt878 PCI TV card: notch filter, dot crawl, software MPEG-2. */
    public static CaptureParams bt878() {
        return of(YC_NOTCH, 1.3f, 1.1f, 0.7f, 0.018f, 0.5f, 0.12f, 0.15f, 4f, 1.05f, LEVELS_MATCHED, 12f, 0.6f, false, DEINT_WEAVE);
    }

    /** Card with a hardware MPEG-2 encoder and a 3-line comb. */
    public static CaptureParams hardwareMpeg2() {
        return of(YC_COMB_3LINE, 1.6f, 1.3f, 0.35f, 0.01f, 0.25f, 0.05f, 0f, 0f, 1.0f, LEVELS_MATCHED, 8f, 0.4f, false, DEINT_WEAVE);
    }

    /** Bargain USB 2.0 dongle: 2-line comb, raised blacks, heavy compression, CUE bug, blended fields. */
    public static CaptureParams usbDongle() {
        return of(YC_COMB_2LINE, 1.2f, 0.8f, 1.0f, 0.03f, 0.9f, 0.2f, 0.3f, -6f, 1.15f, LEVELS_RAISED_BLACKS, 22f, 0.8f, true, DEINT_BLEND);
    }

    /** Near-transparent reference chain, useful for A/B checks. */
    public static CaptureParams clean() {
        return of(YC_COMB_3LINE, 1.6f, 1.3f, 0f, 0f, 0f, 0f, 0f, 0f, 1.0f, LEVELS_MATCHED, 2f, 0f, false, DEINT_WEAVE);
    }
}
