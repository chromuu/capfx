# Composite Capture '05 for Android

A 2005-era composite capture card effect (NTSC dot crawl, chroma bleed, comb-filter artifacts, MPEG-2 blocking, interlacing) as an OpenGL ES 3.0 filter. It works with a still image or any video source that renders into a `Surface` (MediaPlayer, ExoPlayer, CameraX).

## Files

| File | What it is |
|---|---|
| `CaptureShaders.java` | All GLSL, generated from the tested WebGL version. Don't edit by hand. |
| `CaptureCardFilter.java` | The 12-pass pipeline. Use it directly if you already have your own GL renderer. |
| `CaptureParams.java` | Every setting, plus 4 presets. |
| `CaptureCardRenderer.java` | Drop-in `GLSurfaceView.Renderer` for images or video. |

They're Java so they work in Kotlin and Java projects alike.

## Setup

1. Copy the four `.java` files into `app/src/main/java/com/example/capturefx/`. To use your own package, change the `package` line at the top of each file.
2. Add this to `AndroidManifest.xml`, inside `<manifest>`:
   ```xml
   <uses-feature android:glEsVersion="0x00030000" android:required="true" />
   ```
3. Show it from an Activity (Kotlin):

```kotlin
import android.media.MediaPlayer
import android.opengl.GLSurfaceView
import android.os.Bundle
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import com.example.capturefx.CaptureCardRenderer
import com.example.capturefx.CaptureParams

class CaptureActivity : AppCompatActivity() {
    private lateinit var glView: GLSurfaceView
    private lateinit var renderer: CaptureCardRenderer
    private var player: MediaPlayer? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        glView = GLSurfaceView(this)
        glView.setEGLContextClientVersion(3)

        // Still image instead of video:
        // val bmp = BitmapFactory.decodeResource(resources, R.drawable.screenshot)
        // renderer = CaptureCardRenderer.forBitmap(glView, bmp)

        renderer = CaptureCardRenderer.forVideo(glView) { surface ->
            // Called again if the GL context is recreated, so build the player here.
            player?.release()
            player = MediaPlayer.create(this, R.raw.gameplay).apply {
                setSurface(surface)
                isLooping = true
                renderer.setVideoSize(videoWidth, videoHeight)
                start()
            }
        }
        renderer.setErrorListener { msg -> Toast.makeText(this, msg, Toast.LENGTH_LONG).show() }
        renderer.setParams(CaptureParams.bt878())

        glView.setRenderer(renderer)
        setContentView(glView)
    }

    override fun onResume() { super.onResume(); glView.onResume(); player?.start() }
    override fun onPause() { player?.pause(); glView.onPause(); super.onPause() }
    override fun onDestroy() { player?.release(); renderer.release(); super.onDestroy() }
}
```

Put a video at `res/raw/gameplay.mp4`, or swap in your own source.

## Changing settings

Presets: `CaptureParams.bt878()`, `hardwareMpeg2()`, `usbDongle()`, `clean()`.

```kotlin
val p = renderer.params.apply {
    ycMode = CaptureParams.YC_COMB_3LINE   // or YC_NOTCH, YC_COMB_2LINE
    noise = 0.03f                          // 3 IRE of RF noise
    q = 24f                                // heavier MPEG-2 compression
    deint = CaptureParams.DEINT_BOB
}
renderer.setParams(p)                       // safe from the UI thread
```

Set `view = CaptureParams.VIEW_COMPOSITE` (or `VIEW_LUMA`, `VIEW_CHROMA`, `VIEW_DECODED`) to see each stage while tuning. Ranges are in the comments in `CaptureParams.java`.

## Using the filter in your own GL code

```java
filter = new CaptureCardFilter(/* externalInput = */ true);  // true for OES / SurfaceTexture
filter.init();                                            // GL thread, after context creation
filter.submit(texId, texMatrix, width, height);           // each new input frame
filter.process();                                         // at 29.97 Hz
filter.draw(viewWidth, viewHeight);                       // letterboxed 4:3 to framebuffer 0
// or use filter.outputTexture(): 720x480 float RGBA, display at 4:3
```

## Requirements and notes

- Needs OpenGL ES 3.0 and floating-point render targets (`EXT_color_buffer_float`, `EXT_color_buffer_half_float`, or ES 3.2). It uses 32-bit float targets when available and falls back to 16-bit. Without either, the error listener gets a message instead of a crash.
- The NTSC frame clock runs at 29.97 Hz independent of the screen refresh rate, so dot crawl and interlacing behave correctly on 90/120 Hz phones.
- Output is processed at the real capture size (720x480), so cost doesn't grow with screen resolution.
- Verified here: every shader compiles under glslang as GLSL ES 3.00. The Java compiles at Java 8 language level, and the shader text is byte-identical to the WebGL version that passed the artifact tests. It has not been run on a physical device yet.
