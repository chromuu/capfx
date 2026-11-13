package com.example.capturefx;

import android.opengl.GLES11Ext;

import java.util.HashMap;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

import static android.opengl.GLES30.*;

/**
 * 2005-era composite capture card simulation for OpenGL ES 3.0.
 *
 * <p>Pipeline: input → 640x480 console frame → interlaced 720x480 raster → NTSC composite at 4fsc →
 * notch / comb Y/C separation → quadrature demodulation → BT.601 4:2:0 → MPEG-2 intra quantisation →
 * interlaced playback.</p>
 *
 * <p>All methods must be called on the GL thread (inside a GLSurfaceView.Renderer callback).</p>
 *
 * <pre>
 *   filter.init();                                   // onSurfaceCreated
 *   filter.submit(texId, texMatrix, width, height);  // each new input frame (29.97 Hz)
 *   filter.process();                                // each 29.97 Hz tick
 *   filter.draw(viewWidth, viewHeight);              // every onDrawFrame
 * </pre>
 */
public final class CaptureCardFilter {

    /** Texture matrix for a texture uploaded from a Bitmap (row 0 at the top). */
    public static final float[] FLIP_Y = {1, 0, 0, 0, 0, -1, 0, 0, 0, 0, 1, 0, 0, 1, 0, 1};
    /** Texture matrix for a texture that is already bottom-up. */
    public static final float[] IDENTITY = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};

    /** Live parameters. Read on the GL thread every frame. */
    public final CaptureParams params = CaptureParams.bt878();

    private static final float MHZ = 1.897f; // gaussian sigma (4fsc samples) = 1.897 / (-3 dB point in MHz)

    private final boolean externalInput;
    private int frame = 0;
    private int floatInternal, floatType;
    private String floatName = "";
    private int vao;

    private Prog pCopy, pSrc, pEnc, pYc1, pYc2, pDem, pToY, pToC, pDct, pOut, pPre;
    private Target rawCur, rawPrev, tSrc, tComp, tYc1, tYc2, tDec, tYA, tYB, tCA, tCB, tOut;
    private boolean hasInput = false;

    /**
     * @param externalInput true if the input is a GL_TEXTURE_EXTERNAL_OES texture
     *                      (SurfaceTexture: MediaPlayer, ExoPlayer, camera), false for a GL_TEXTURE_2D.
     */
    public CaptureCardFilter(boolean externalInput) {
        this.externalInput = externalInput;
    }

    // ------------------------------------------------------------------ setup

    /** Compiles shaders and allocates render targets. Throws IllegalStateException if the GPU can't run it. */
    public void init() {
        frame = 0;
        hasInput = false;
        pickFloatFormat();

        pCopy = new Prog("copy", externalInput ? CaptureShaders.FS_COPY_OES : CaptureShaders.FS_COPY_2D);
        pSrc = new Prog("src", CaptureShaders.FS_SRC);
        pEnc = new Prog("enc", CaptureShaders.FS_ENC);
        pYc1 = new Prog("yc1", CaptureShaders.FS_YC1);
        pYc2 = new Prog("yc2", CaptureShaders.FS_YC2);
        pDem = new Prog("dem", CaptureShaders.FS_DEM);
        pToY = new Prog("toy", CaptureShaders.FS_TOY);
        pToC = new Prog("toc", CaptureShaders.FS_TOC);
        pDct = new Prog("dct", CaptureShaders.FS_DCT);
        pOut = new Prog("out", CaptureShaders.FS_OUT);
        pPre = new Prog("pre", CaptureShaders.FS_PRE);

        rawCur = byteTarget(640, 480);
        rawPrev = byteTarget(640, 480);
        tSrc = floatTarget(720, 480);
        tComp = floatTarget(764, 480);
        tYc1 = floatTarget(764, 480);
        tYc2 = floatTarget(764, 480);
        tDec = floatTarget(720, 480);
        tYA = floatTarget(720, 480);
        tYB = floatTarget(720, 480);
        tCA = floatTarget(360, 240);
        tCB = floatTarget(360, 240);
        tOut = floatTarget(720, 480);

        int[] a = new int[1];
        glGenVertexArrays(1, a, 0);
        vao = a[0];
        glDisable(GL_BLEND);
        glDisable(GL_DEPTH_TEST);
        glDisable(GL_DITHER);
    }

    /** Float render-target format in use, e.g. "RGBA32F". */
    public String floatFormat() { return floatName; }

    /** Current frame number (advances once per {@link #process()}). */
    public int frame() { return frame; }

    /** The processed 720x480 frame (float RGBA, non-square pixels; display it 4:3). */
    public int outputTexture() { return tOut.tex; }

    /** Frees all GL objects. Call on the GL thread; not needed after the context is lost. */
    public void release() {
        Prog[] ps = {pCopy, pSrc, pEnc, pYc1, pYc2, pDem, pToY, pToC, pDct, pOut, pPre};
        for (Prog p : ps) if (p != null) glDeleteProgram(p.id);
        Target[] ts = {rawCur, rawPrev, tSrc, tComp, tYc1, tYc2, tDec, tYA, tYB, tCA, tCB, tOut};
        for (Target t : ts) if (t != null) t.delete();
        if (vao != 0) glDeleteVertexArrays(1, new int[]{vao}, 0);
        vao = 0;
    }

    // ------------------------------------------------------------------ per frame

    /**
     * Feeds a new picture from the console. The previous one becomes the earlier field, so motion combs
     * the way a real interlaced capture does. Call once per new video frame, or once for a still image.
     *
     * @param texId     GL_TEXTURE_2D or GL_TEXTURE_EXTERNAL_OES texture (matching the constructor)
     * @param texMatrix 4x4 column-major texture matrix: SurfaceTexture.getTransformMatrix() or FLIP_Y for a Bitmap
     * @param width     input width in pixels (for 4:3 letterboxing)
     * @param height    input height in pixels
     */
    public void submit(int texId, float[] texMatrix, int width, int height) {
        Target t = rawPrev; rawPrev = rawCur; rawCur = t;
        float a = (float) width / Math.max(1, height), frameAspect = 4f / 3f;
        float fitX = a > frameAspect ? 1f : a / frameAspect;
        float fitY = a > frameAspect ? frameAspect / a : 1f;
        copyInto(rawCur, texId, texMatrix, fitX, fitY);
        if (!hasInput) { copyInto(rawPrev, texId, texMatrix, fitX, fitY); hasInput = true; }
    }

    private void copyInto(Target dst, int texId, float[] m, float fitX, float fitY) {
        begin(pCopy, dst);
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(externalInput ? GLES11Ext.GL_TEXTURE_EXTERNAL_OES : GL_TEXTURE_2D, texId);
        glUniform1i(pCopy.loc("uIn"), 0);
        glUniformMatrix4fv(pCopy.loc("uTexM"), 1, false, m, 0);
        u2(pCopy, "uFit", fitX, fitY);
        drawTri();
    }

    /** Advances one NTSC frame (call at 29.97 Hz) and runs the whole chain. */
    public void process() {
        if (!hasInput) return;
        frame++;
        final CaptureParams P = params;
        final float t = frame / 29.97f, rad = (float) (Math.PI / 180.0);
        final float setupEnc = P.levels == CaptureParams.LEVELS_CRUSHED_BLACKS ? 0f : 0.075f;
        final float setupDec = P.levels == CaptureParams.LEVELS_RAISED_BLACKS ? 0f : 0.075f;

        // 1. weave two console frames into the 720x480 capture raster
        begin(pSrc, tSrc); tex(pSrc, "uA", 0, rawPrev.tex); tex(pSrc, "uB", 1, rawCur.tex); drawTri();

        // 2. composite encoder + cable impairments
        begin(pEnc, tComp); tex(pEnc, "uSrc", 0, tSrc.tex);
        i1(pEnc, "uFrame", frame); f1(pEnc, "uTime", t); f1(pEnc, "uNoise", P.noise); f1(pEnc, "uJitter", P.jitter);
        f1(pEnc, "uHum", P.hum); f1(pEnc, "uBeat", P.beat); f1(pEnc, "uHue", P.hue * rad); f1(pEnc, "uSetup", setupEnc);
        f1(pEnc, "uSigY", MHZ / 4.2f); f1(pEnc, "uSigI", MHZ / 1.3f); f1(pEnc, "uSigQ", MHZ / 0.5f);
        drawTri();

        // 3. Y/C separation (band width only shapes the notch)
        begin(pYc1, tYc1); tex(pYc1, "uComp", 0, tComp.tex); i1(pYc1, "uMode", P.ycMode); drawTri();
        begin(pYc2, tYc2); tex(pYc2, "uT", 0, tYc1.tex);
        f1(pYc2, "uSigB", P.ycMode == CaptureParams.YC_NOTCH ? sig(P.ycMHz) : 0f); drawTri();

        // 4. demodulate, peaking, levels, ADC clip
        begin(pDem, tDec); tex(pDem, "uYC", 0, tYc2.tex);
        i1(pDem, "uFrame", frame); f1(pDem, "uSigD", sig(P.chromaMHz)); f1(pDem, "uPeak", P.peak);
        f1(pDem, "uSat", P.sat); f1(pDem, "uTint", -P.tint * rad); f1(pDem, "uSetupDec", setupDec);
        drawTri();

        // 5. BT.601 luma plane and field-based 4:2:0 chroma plane
        begin(pToY, tYA); tex(pToY, "uT", 0, tDec.tex); drawTri();
        begin(pToC, tCA); tex(pToC, "uT", 0, tDec.tex); drawTri();

        // 6. MPEG-2: forward DCT (H, V + quantise), inverse DCT (V, H); result lands back in A
        if (P.mpeg) {
            float q = qEff();
            dct(tYA, tYB, q);
            dct(tCA, tCB, q);
        }

        // 7. 4:2:0 upsampling, YCbCr -> RGB, deinterlace
        begin(pOut, tOut); tex(pOut, "uY", 0, tYA.tex); tex(pOut, "uC", 1, tCA.tex);
        i1(pOut, "uCUE", P.cue ? 1 : 0); i1(pOut, "uDeint", P.deint); i1(pOut, "uFrame", frame);
        drawTri();
    }

    private void dct(Target a, Target b, float q) {
        dctPass(a, b, 0, 0, 0, q);
        dctPass(b, a, 1, 0, 1, q);
        dctPass(a, b, 1, 1, 0, q);
        dctPass(b, a, 0, 1, 0, q);
    }

    private void dctPass(Target in, Target out, int dir, int inv, int quant, float q) {
        begin(pDct, out); tex(pDct, "uT", 0, in.tex);
        i1(pDct, "uDir", dir); i1(pDct, "uInv", inv); i1(pDct, "uQuant", quant); f1(pDct, "uQ", q);
        drawTri();
    }

    /** Effective quantiser scale this frame (rises across the 15-frame GOP when pumping is on). */
    public float qEff() {
        return params.q * (1f + params.pump * 0.7f * ((frame % 15) / 14f));
    }

    /**
     * Draws the selected stage into the current default framebuffer, letterboxed to 4:3.
     * Clears the whole view to black first.
     */
    public void draw(int viewWidth, int viewHeight) {
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        glViewport(0, 0, viewWidth, viewHeight);
        glClearColor(0f, 0f, 0f, 1f);
        glClear(GL_COLOR_BUFFER_BIT);
        if (!hasInput || frame == 0) return;

        int w, h;
        if (viewWidth * 3 > viewHeight * 4) { h = viewHeight; w = viewHeight * 4 / 3; }
        else { w = viewWidth; h = viewWidth * 3 / 4; }
        int x0 = (viewWidth - w) / 2, y0 = (viewHeight - h) / 2;

        Target stage; int kind;
        switch (params.view) {
            case CaptureParams.VIEW_SOURCE: stage = tSrc; kind = 0; break;
            case CaptureParams.VIEW_COMPOSITE: stage = tComp; kind = 1; break;
            case CaptureParams.VIEW_LUMA: stage = tYc2; kind = 2; break;
            case CaptureParams.VIEW_CHROMA: stage = tYc2; kind = 3; break;
            case CaptureParams.VIEW_DECODED: stage = tDec; kind = 0; break;
            default: stage = tOut; kind = 0; break;
        }
        glUseProgram(pPre.id);
        glBindVertexArray(vao);
        glViewport(x0, y0, w, h);
        tex(pPre, "uT", 0, stage.tex); tex(pPre, "uRef", 1, tSrc.tex);
        i1(pPre, "uKind", kind); f1(pPre, "uOver", params.overscan); f1(pPre, "uSplit", -1f);
        u2(pPre, "uCanvas", w, h); u2(pPre, "uOrigin", x0, y0);
        drawTri();
    }

    // ------------------------------------------------------------------ GL helpers

    private static float sig(float mhz) { return Math.max(0.6f, MHZ / mhz); }

    private void begin(Prog p, Target dst) {
        glUseProgram(p.id);
        glBindVertexArray(vao);
        glBindFramebuffer(GL_FRAMEBUFFER, dst.fbo);
        glViewport(0, 0, dst.w, dst.h);
    }

    private static void tex(Prog p, String name, int unit, int texId) {
        glActiveTexture(GL_TEXTURE0 + unit);
        glBindTexture(GL_TEXTURE_2D, texId);
        glUniform1i(p.loc(name), unit);
    }

    private static void i1(Prog p, String n, int v) { glUniform1i(p.loc(n), v); }
    private static void f1(Prog p, String n, float v) { glUniform1f(p.loc(n), v); }
    private static void u2(Prog p, String n, float x, float y) { glUniform2f(p.loc(n), x, y); }
    private static void drawTri() { glDrawArrays(GL_TRIANGLES, 0, 3); }

    private void pickFloatFormat() {
        String ext = glGetString(GL_EXTENSIONS);
        if (ext == null) ext = "";
        String ver = glGetString(GL_VERSION);
        boolean es32 = false;
        Matcher m = Pattern.compile("OpenGL ES (\\d+)\\.(\\d+)").matcher(ver == null ? "" : ver);
        if (m.find()) {
            int major = Integer.parseInt(m.group(1)), minor = Integer.parseInt(m.group(2));
            es32 = major > 3 || (major == 3 && minor >= 2);
        }
        if ((es32 || ext.contains("GL_EXT_color_buffer_float")) && probe(GL_RGBA32F, GL_FLOAT)) {
            floatInternal = GL_RGBA32F; floatType = GL_FLOAT; floatName = "RGBA32F"; return;
        }
        if ((es32 || ext.contains("GL_EXT_color_buffer_float") || ext.contains("GL_EXT_color_buffer_half_float"))
                && probe(GL_RGBA16F, GL_HALF_FLOAT)) {
            floatInternal = GL_RGBA16F; floatType = GL_HALF_FLOAT; floatName = "RGBA16F"; return;
        }
        throw new IllegalStateException("This GPU can't render to floating-point textures, which the signal simulation needs.");
    }

    private static boolean probe(int internal, int type) {
        Target t = Target.create(8, 8, internal, type, GL_NEAREST);
        if (t == null) return false;
        t.delete();
        return true;
    }

    private Target floatTarget(int w, int h) {
        Target t = Target.create(w, h, floatInternal, floatType, GL_NEAREST);
        if (t == null) throw new IllegalStateException("Framebuffer " + w + "x" + h + " " + floatName + " is incomplete.");
        return t;
    }

    private static Target byteTarget(int w, int h) {
        Target t = Target.create(w, h, GL_RGBA8, GL_UNSIGNED_BYTE, GL_LINEAR);
        if (t == null) throw new IllegalStateException("Framebuffer " + w + "x" + h + " RGBA8 is incomplete.");
        return t;
    }

    private static final class Target {
        final int tex, fbo, w, h;

        private Target(int tex, int fbo, int w, int h) { this.tex = tex; this.fbo = fbo; this.w = w; this.h = h; }

        static Target create(int w, int h, int internal, int type, int filter) {
            int[] a = new int[1];
            glGenTextures(1, a, 0);
            int tex = a[0];
            glBindTexture(GL_TEXTURE_2D, tex);
            glTexImage2D(GL_TEXTURE_2D, 0, internal, w, h, 0, GL_RGBA, type, null);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, filter);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, filter);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            glGenFramebuffers(1, a, 0);
            int fbo = a[0];
            glBindFramebuffer(GL_FRAMEBUFFER, fbo);
            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
            int status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            for (int i = 0; i < 16 && glGetError() != GL_NO_ERROR; i++) { /* drain errors from an unsupported format */ }
            if (status != GL_FRAMEBUFFER_COMPLETE) {
                glDeleteFramebuffers(1, new int[]{fbo}, 0);
                glDeleteTextures(1, new int[]{tex}, 0);
                return null;
            }
            return new Target(tex, fbo, w, h);
        }

        void delete() {
            glDeleteFramebuffers(1, new int[]{fbo}, 0);
            glDeleteTextures(1, new int[]{tex}, 0);
        }
    }

    private static final class Prog {
        final int id;
        private final HashMap<String, Integer> locs = new HashMap<>();

        Prog(String name, String fragmentSrc) {
            int vs = compile(name, GL_VERTEX_SHADER, CaptureShaders.VS);
            int fs = compile(name, GL_FRAGMENT_SHADER, fragmentSrc);
            id = glCreateProgram();
            glAttachShader(id, vs);
            glAttachShader(id, fs);
            glLinkProgram(id);
            int[] ok = new int[1];
            glGetProgramiv(id, GL_LINK_STATUS, ok, 0);
            glDeleteShader(vs);
            glDeleteShader(fs);
            if (ok[0] == 0) {
                String log = glGetProgramInfoLog(id);
                glDeleteProgram(id);
                throw new IllegalStateException("Link failed (" + name + "): " + log);
            }
        }

        int loc(String n) {
            Integer l = locs.get(n);
            if (l == null) { l = glGetUniformLocation(id, n); locs.put(n, l); }
            return l;
        }

        private static int compile(String name, int type, String src) {
            int s = glCreateShader(type);
            glShaderSource(s, src);
            glCompileShader(s);
            int[] ok = new int[1];
            glGetShaderiv(s, GL_COMPILE_STATUS, ok, 0);
            if (ok[0] == 0) {
                String log = glGetShaderInfoLog(s);
                glDeleteShader(s);
                throw new IllegalStateException("Compile failed (" + name + "): " + log);
            }
            return s;
        }
    }
}
