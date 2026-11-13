package com.example.capturefx;

import android.graphics.Bitmap;
import android.graphics.SurfaceTexture;
import android.opengl.GLES11Ext;
import android.opengl.GLSurfaceView;
import android.opengl.GLUtils;
import android.util.Log;
import android.view.Surface;

import javax.microedition.khronos.egl.EGLConfig;
import javax.microedition.khronos.opengles.GL10;

import static android.opengl.GLES30.*;

/**
 * Ready-made GLSurfaceView renderer around {@link CaptureCardFilter}.
 *
 * <p>Still image: {@code CaptureCardRenderer.forBitmap(glView, bitmap)}.<br>
 * Video / camera: {@code CaptureCardRenderer.forVideo(glView, surface -> player.setSurface(surface))},
 * then call {@link #setVideoSize(int, int)} once the player knows the size.</p>
 *
 * <p>Call {@code glView.setEGLContextClientVersion(3)} before {@code setRenderer}.</p>
 */
public final class CaptureCardRenderer implements GLSurfaceView.Renderer, SurfaceTexture.OnFrameAvailableListener {

    /** Receives the Surface to hand to MediaPlayer / ExoPlayer / CameraX. Called on the main thread. */
    public interface SurfaceListener {
        void onSurfaceReady(Surface surface);
    }

    /** Reports GPU setup failures (e.g. no float render targets). Called on the main thread. */
    public interface ErrorListener {
        void onError(String message);
    }

    private static final String TAG = "CaptureCard";
    private static final long FRAME_NS = (long) (1_000_000_000L / 29.97);

    private final GLSurfaceView view;
    private final boolean video;
    private final SurfaceListener surfaceListener;
    private Bitmap bitmap;
    private ErrorListener errorListener;

    private CaptureCardFilter filter;
    private final CaptureParams pending = CaptureParams.bt878();
    private volatile boolean paramsDirty = true;

    private int inputTex;
    private SurfaceTexture surfaceTexture;
    private Surface surface;
    private final float[] texMatrix = new float[16];
    private volatile boolean frameAvailable;
    private boolean newFrameSinceTick;
    private volatile int videoW = 640, videoH = 480;
    private boolean bitmapDirty = true;

    private int viewW = 1, viewH = 1;
    private long lastNs;
    private long accNs;
    private boolean failed;

    private CaptureCardRenderer(GLSurfaceView view, boolean video, Bitmap bitmap, SurfaceListener listener) {
        this.view = view;
        this.video = video;
        this.bitmap = bitmap;
        this.surfaceListener = listener;
    }

    /** Processes a still image (a screenshot, a test card). The noise, crawl and GOP still animate. */
    public static CaptureCardRenderer forBitmap(GLSurfaceView view, Bitmap bitmap) {
        return new CaptureCardRenderer(view, false, bitmap, null);
    }

    /** Processes video from anything that renders into a Surface. */
    public static CaptureCardRenderer forVideo(GLSurfaceView view, SurfaceListener listener) {
        return new CaptureCardRenderer(view, true, null, listener);
    }

    public void setErrorListener(ErrorListener l) { errorListener = l; }

    /** Video size in pixels, for 4:3 letterboxing. Safe to call from any thread. */
    public void setVideoSize(int width, int height) {
        if (width > 0 && height > 0) { videoW = width; videoH = height; }
    }

    /** Replaces the image shown in bitmap mode. Call from the UI thread. */
    public void setBitmap(final Bitmap b) {
        view.queueEvent(new Runnable() {
            @Override public void run() { bitmap = b; bitmapDirty = true; }
        });
    }

    /** Applies new settings (copied, so the caller can keep editing its own instance). Any thread. */
    public void setParams(CaptureParams p) {
        synchronized (pending) { pending.set(p); }
        paramsDirty = true;
    }

    /** A copy of the current settings. */
    public CaptureParams getParams() {
        synchronized (pending) { return pending.copy(); }
    }

    // ------------------------------------------------------------------ GLSurfaceView.Renderer

    @Override
    public void onSurfaceCreated(GL10 unused, EGLConfig config) {
        // A new context means every GL object from before is gone.
        failed = false;
        bitmapDirty = true;
        paramsDirty = true;
        lastNs = 0; accNs = 0;
        try {
            filter = new CaptureCardFilter(video);
            filter.init();
            Log.i(TAG, "float targets: " + filter.floatFormat());
        } catch (RuntimeException e) {
            fail(e.getMessage());
            return;
        }

        int[] a = new int[1];
        glGenTextures(1, a, 0);
        inputTex = a[0];
        int target = video ? GLES11Ext.GL_TEXTURE_EXTERNAL_OES : GL_TEXTURE_2D;
        glBindTexture(target, inputTex);
        glTexParameteri(target, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(target, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(target, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(target, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);

        if (video) {
            if (surface != null) surface.release();
            if (surfaceTexture != null) surfaceTexture.release();
            surfaceTexture = new SurfaceTexture(inputTex);
            surfaceTexture.setOnFrameAvailableListener(this);
            surface = new Surface(surfaceTexture);
            final Surface s = surface;
            if (surfaceListener != null) view.post(new Runnable() {
                @Override public void run() { surfaceListener.onSurfaceReady(s); }
            });
        }
    }

    @Override
    public void onSurfaceChanged(GL10 unused, int width, int height) {
        viewW = width;
        viewH = height;
    }

    @Override
    public void onDrawFrame(GL10 unused) {
        if (failed || filter == null) {
            glClearColor(0f, 0f, 0f, 1f);
            glClear(GL_COLOR_BUFFER_BIT);
            return;
        }
        if (paramsDirty) {
            synchronized (pending) { filter.params.set(pending); }
            paramsDirty = false;
        }

        if (video && frameAvailable) {
            frameAvailable = false;
            surfaceTexture.updateTexImage();
            surfaceTexture.getTransformMatrix(texMatrix);
            newFrameSinceTick = true;
        }
        if (!video && bitmapDirty && bitmap != null) {
            glBindTexture(GL_TEXTURE_2D, inputTex);
            GLUtils.texImage2D(GL_TEXTURE_2D, 0, bitmap, 0);
            bitmapDirty = false;
            filter.submit(inputTex, CaptureCardFilter.FLIP_Y, bitmap.getWidth(), bitmap.getHeight());
            filter.submit(inputTex, CaptureCardFilter.FLIP_Y, bitmap.getWidth(), bitmap.getHeight()); // both fields
        }

        // Tick the NTSC frame clock at 29.97 Hz regardless of the display refresh rate.
        long now = System.nanoTime();
        if (lastNs == 0) { lastNs = now; accNs = FRAME_NS; }
        accNs += now - lastNs;
        lastNs = now;
        if (accNs > 4 * FRAME_NS) accNs = FRAME_NS;
        if (accNs >= FRAME_NS) {
            accNs -= FRAME_NS;
            if (video && newFrameSinceTick) {
                filter.submit(inputTex, texMatrix, videoW, videoH);
                newFrameSinceTick = false;
            }
            filter.process();
        }
        filter.draw(viewW, viewH);
    }

    @Override
    public void onFrameAvailable(SurfaceTexture st) {
        frameAvailable = true; // GLSurfaceView in continuous mode picks it up on the next draw
    }

    /** Releases the video Surface. Call from Activity.onDestroy after stopping the player. */
    public void release() {
        view.queueEvent(new Runnable() {
            @Override public void run() {
                if (filter != null) filter.release();
                filter = null;
                if (surface != null) surface.release();
                if (surfaceTexture != null) surfaceTexture.release();
                surface = null;
                surfaceTexture = null;
            }
        });
    }

    private void fail(final String message) {
        failed = true;
        Log.e(TAG, message);
        if (errorListener != null) view.post(new Runnable() {
            @Override public void run() { errorListener.onError(message); }
        });
    }
}
