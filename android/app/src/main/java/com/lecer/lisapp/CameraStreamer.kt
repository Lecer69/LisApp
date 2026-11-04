package com.lecer.lisapp

import android.annotation.SuppressLint
import android.content.Context
import android.hardware.camera2.CameraAccessException
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CameraMetadata
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log
import android.util.Range
import android.util.Size
import android.view.OrientationEventListener
import android.view.Surface
import androidx.core.content.getSystemService
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import uniffi.lis_ffi.VideoConfig

/** 16:9, largest first. */
val CAMERA_SIZES = listOf(
    Size(3840, 2160),
    Size(2560, 1440),
    Size(1920, 1080),
    Size(1280, 720),
    Size(960, 540),
    Size(640, 360),
)
val DEFAULT_CAMERA_SIZE = Size(1920, 1080)

val Size.label: String
    get() = if (height == 2160) "4K" else "${height}p"

/** The size asked for, or the next one down when this camera doesn't have it. */
fun closestSize(sizes: List<Size>, wanted: Size): Size? =
    sizes.firstOrNull { it.height <= wanted.height } ?: sizes.lastOrNull()

/** Which way round the picture goes in OBS. A locked one still turns over, so it's never upside down. */
enum class CameraAspect(val label: String) {
    AUTO("Auto-rotate"),
    LANDSCAPE("Lock landscape"),
    PORTRAIT("Lock portrait"),
}

/** The camera, through the hardware encoder, to the PC. The activity stops it as soon as it leaves the screen. */
class CameraStreamer(private val context: Context) {
    private val manager = context.getSystemService<CameraManager>()
    private var thread: HandlerThread? = null
    private var handler: Handler? = null

    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var encoder: MediaCodec? = null
    private var encoderInput: Surface? = null
    private var orientationListener: OrientationEventListener? = null

    private var config: VideoConfig? = null
    private var sensorOrientation = 0
    private var isFront = false
    @Volatile private var aspect = CameraAspect.AUTO
    private var phoneDegrees = 0
    private var portraitDegrees = 0
    // The sensor's own way up on nearly every phone, so a landscape lock starts unturned
    private var landscapeDegrees = 270
    private var codecConfig = ByteArray(0)
    private var lastKeyframeRequest = 0L

    private var windowStart = 0L
    private var windowFrames = 0
    private var windowBytes = 0L

    fun start(preview: Surface, front: Boolean, wantedSize: Size, wantedFps: Int) {
        stop()
        val cameraThread = HandlerThread("LisCamera").also { it.start() }
        thread = cameraThread
        handler = Handler(cameraThread.looper).also { cameraHandler ->
            cameraHandler.post { open(preview, front, wantedSize, wantedFps, cameraHandler) }
        }
        Lis.update { copy(camera = CameraStatus(running = true, front = front)) }
    }

    /** Applies at once, including mid stream. */
    fun setAspect(wanted: CameraAspect) {
        aspect = wanted
        handler?.post(::sendRotation)
    }

    /** What this camera and an encoder can both do. */
    fun sizes(front: Boolean): List<Size> = try {
        val map = cameraId(front)
            ?.let { manager?.getCameraCharacteristics(it) }
            ?.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
        val offered = map?.getOutputSizes(MediaCodec::class.java).orEmpty()
        val encoders = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
            .filter { it.isEncoder && MediaFormat.MIMETYPE_VIDEO_AVC in it.supportedTypes }
            .mapNotNull { it.getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC).videoCapabilities }
        CAMERA_SIZES.filter { size -> size in offered && encoders.any { it.isSizeSupported(size.width, size.height) } }
    } catch (error: CameraAccessException) {
        Log.w(TAG, "can't read the camera's sizes", error)
        emptyList()
    }

    private fun cameraId(front: Boolean): String? {
        val cameras = manager ?: return null
        val facing = if (front) CameraMetadata.LENS_FACING_FRONT else CameraMetadata.LENS_FACING_BACK
        return cameras.cameraIdList.firstOrNull { cameras.getCameraCharacteristics(it)[CameraCharacteristics.LENS_FACING] == facing }
    }

    /** Waits for the camera to close, so nothing runs once the app has left the screen. */
    fun stop() {
        val cameraHandler = handler ?: return
        val done = CountDownLatch(1)
        cameraHandler.post {
            release()
            done.countDown()
        }
        done.await(STOP_WAIT_SECONDS, TimeUnit.SECONDS)
        thread?.quitSafely()
        thread = null
        handler = null
        Lis.update { copy(camera = CameraStatus(front = camera.front, error = camera.error)) }
    }

    private fun fail(message: String) {
        Log.w(TAG, message)
        release()
        Lis.update { copy(camera = camera.copy(running = false, error = message)) }
    }

    private fun release() {
        orientationListener?.disable()
        orientationListener = null
        runCatching { session?.close() }
        session = null
        runCatching { device?.close() }
        device = null
        val closing = encoder
        encoder = null
        runCatching { closing?.stop() }
        runCatching { closing?.release() }
        encoderInput?.release()
        encoderInput = null
        codecConfig = ByteArray(0)
        config = null
        Lis.client.stopVideo()
    }

    /** The fastest range at or below what was asked for, with the steadiest frame rate. */
    private fun pickFrameRate(characteristics: CameraCharacteristics, size: Size, wanted: Int): Range<Int>? {
        val map = characteristics[CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP] ?: return null
        val ranges = characteristics[CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES] ?: return null
        val minFrameTime = map.getOutputMinFrameDuration(MediaCodec::class.java, size)
        val picked = listOf(wanted, FALLBACK_FPS).firstNotNullOfOrNull { fps ->
            // Cameras round this up or down, so 60 fps can read as 16666667 ns
            if (minFrameTime * fps > NANOS_PER_SECOND * FRAME_TIME_SLACK_PERCENT / 100) {
                null
            } else {
                ranges.filter { it.upper == fps }.maxByOrNull { it.lower }
            }
        }
        Log.i(TAG, "$size: $minFrameTime ns a frame at best, ranges ${ranges.toList()}, wanted $wanted, picked $picked")
        return picked
    }

    /** Scaled by pixels from what suits 1080p, within what Wi-Fi can be expected to carry. */
    private fun bitrateFor(size: Size, fps: Int): Int {
        val at1080p = if (fps > FALLBACK_FPS) BITRATE_60 else BITRATE_30
        return (at1080p * size.width.toLong() * size.height / PIXELS_1080P).coerceIn(MIN_BITRATE, MAX_BITRATE).toInt()
    }

    @SuppressLint("MissingPermission")
    private fun open(preview: Surface, front: Boolean, wantedSize: Size, wantedFps: Int, cameraHandler: Handler) {
        try {
            val cameras = manager ?: return fail("This phone has no camera service")
            val id = cameraId(front) ?: return fail("No ${if (front) "front" else "back"} camera")
            val size = closestSize(sizes(front), wantedSize) ?: return fail("This camera can't record video")

            val characteristics = cameras.getCameraCharacteristics(id)
            val range = pickFrameRate(characteristics, size, wantedFps)
                ?: return fail("This camera has no usable frame rate at ${size.label}")

            sensorOrientation = characteristics[CameraCharacteristics.SENSOR_ORIENTATION] ?: 0
            isFront = front
            val fps = range.upper
            val bitrate = bitrateFor(size, fps)
            val started = VideoConfig(
                width = size.width.toUInt(),
                height = size.height.toUInt(),
                fps = fps.toUInt(),
                bitrate = bitrate.toUInt(),
                rotation = rotationFor(shownDegrees()).toUInt(),
                camera = if (front) "Front" else "Back",
            )
            config = started

            val input = startEncoder(size, fps, bitrate, cameraHandler)
            encoderInput = input
            Lis.client.startVideo(started)
            watchOrientation(cameraHandler)
            Lis.update { copy(camera = camera.copy(size = size, targetFps = fps, lowestFps = range.lower)) }

            // A display running slower would hold the camera back, as it waits for preview buffers
            runCatching { preview.setFrameRate(fps.toFloat(), Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE) }

            cameras.openCamera(id, object : CameraDevice.StateCallback() {
                override fun onOpened(opened: CameraDevice) {
                    device = opened
                    startSession(opened, preview, input, range, cameraHandler)
                }

                override fun onDisconnected(closed: CameraDevice) {
                    fail("Another app took the camera")
                }

                override fun onError(closed: CameraDevice, error: Int) {
                    fail("The camera stopped working (error $error)")
                }
            }, cameraHandler)
        } catch (error: CameraAccessException) {
            fail("Can't use the camera: ${error.message}")
        } catch (error: RuntimeException) {
            fail("Can't start the camera: ${error.message}")
        }
    }

    private fun format(size: Size, fps: Int, bitrate: Int, tuned: Boolean): MediaFormat =
        MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, size.width, size.height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, bitrate)
            setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, KEYFRAME_SECONDS)
            if (tuned) {
                setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR)
                setInteger(MediaFormat.KEY_PRIORITY, 0)
                setInteger(MediaFormat.KEY_OPERATING_RATE, fps)
                setInteger(MediaFormat.KEY_LATENCY, 1)
                setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
                setInteger(MediaFormat.KEY_COLOR_STANDARD, MediaFormat.COLOR_STANDARD_BT709)
                setInteger(MediaFormat.KEY_COLOR_RANGE, MediaFormat.COLOR_RANGE_LIMITED)
                setInteger(MediaFormat.KEY_COLOR_TRANSFER, MediaFormat.COLOR_TRANSFER_SDR_VIDEO)
            }
        }

    /** Encoders differ in what they accept, so it falls back to the plain settings. */
    private fun startEncoder(size: Size, fps: Int, bitrate: Int, cameraHandler: Handler): Surface {
        val codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        encoder = codec
        codec.setCallback(encoderCallback, cameraHandler)
        try {
            codec.configure(format(size, fps, bitrate, tuned = true), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        } catch (error: RuntimeException) {
            Log.w(TAG, "the encoder refused the tuned settings", error)
            codec.reset()
            codec.setCallback(encoderCallback, cameraHandler)
            codec.configure(format(size, fps, bitrate, tuned = false), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        }
        Log.i(TAG, "encoder ${codec.name}: $size at $fps fps, $bitrate bps")
        val input = codec.createInputSurface()
        codec.start()
        windowStart = SystemClock.elapsedRealtime()
        windowFrames = 0
        windowBytes = 0
        return input
    }

    private fun startSession(
        opened: CameraDevice,
        preview: Surface,
        input: Surface,
        range: Range<Int>,
        cameraHandler: Handler,
    ) {
        try {
            val request = opened.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
                addTarget(preview)
                addTarget(input)
                set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, range)
                // Stabilizing delays frames, and can cap the frame rate
                set(CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE, CameraMetadata.CONTROL_VIDEO_STABILIZATION_MODE_OFF)
            }.build()
            val outputs = listOf(OutputConfiguration(preview), OutputConfiguration(input))
            val callback = object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(configured: CameraCaptureSession) {
                    session = configured
                    try {
                        configured.setRepeatingRequest(request, null, cameraHandler)
                    } catch (error: CameraAccessException) {
                        fail("Can't start the camera: ${error.message}")
                    } catch (error: IllegalStateException) {
                        Log.w(TAG, "the camera closed while starting", error)
                    }
                }

                override fun onConfigureFailed(configured: CameraCaptureSession) {
                    fail("This camera can't run this resolution at ${range.upper} fps. Try less.")
                }
            }
            val setup = SessionConfiguration(SessionConfiguration.SESSION_REGULAR, outputs, { cameraHandler.post(it) }, callback)
            // Many cameras choose their sensor mode here, from the frame rate, and not from later requests
            setup.sessionParameters = request
            opened.createCaptureSession(setup)
        } catch (error: CameraAccessException) {
            fail("Can't start the camera: ${error.message}")
        } catch (error: IllegalStateException) {
            Log.w(TAG, "the camera closed while starting", error)
        }
    }

    private val encoderCallback = object : MediaCodec.Callback() {
        override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {}

        override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {}

        override fun onError(codec: MediaCodec, error: MediaCodec.CodecException) {
            if (codec === encoder) fail("The video encoder stopped: ${error.diagnosticInfo}")
        }

        override fun onOutputBufferAvailable(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            if (codec !== encoder) return
            try {
                val buffer = codec.getOutputBuffer(index)
                if (buffer != null && info.size > 0) {
                    val bytes = ByteArray(info.size)
                    buffer.position(info.offset)
                    buffer.get(bytes)
                    if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) {
                        codecConfig = bytes
                    } else {
                        send(codec, bytes, info)
                    }
                }
                codec.releaseOutputBuffer(index, false)
            } catch (error: IllegalStateException) {
                Log.w(TAG, "the encoder closed mid frame", error)
            }
        }
    }

    /** Headers go before every keyframe, so a viewer can join or recover at any of them. */
    private fun send(codec: MediaCodec, frame: ByteArray, info: MediaCodec.BufferInfo) {
        val keyframe = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
        val data = if (keyframe) codecConfig + frame else frame
        val feedback = Lis.client.sendVideoFrame(data, info.presentationTimeUs, keyframe)

        val now = SystemClock.elapsedRealtime()
        if (feedback.needKeyframe && now - lastKeyframeRequest > KEYFRAME_REQUEST_MS) {
            lastKeyframeRequest = now
            codec.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
        }
        feedback.bitrate?.let { changeBitrate(codec, it.toInt()) }

        windowFrames++
        windowBytes += data.size
        val elapsed = now - windowStart
        if (elapsed >= STATS_EVERY_MS) {
            val fps = (windowFrames * 1000L / elapsed).toInt()
            val mbps = windowBytes * 8f / 1000f / elapsed
            windowStart = now
            windowFrames = 0
            windowBytes = 0
            Lis.update { copy(camera = camera.copy(fps = fps, mbps = mbps)) }
        }
    }

    /** Takes effect from the next frames, with no restart of the camera or the stream. */
    private fun changeBitrate(codec: MediaCodec, bitrate: Int) {
        codec.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, bitrate) })
        val current = config ?: return
        Log.i(TAG, "bitrate now $bitrate bps, chosen ${current.bitrate}")
        Lis.update { copy(camera = camera.copy(limited = bitrate.toUInt() < current.bitrate)) }
    }

    /** Degrees clockwise to show the frames upright, as Camera2 documents it. */
    private fun rotationFor(deviceDegrees: Int): Int =
        if (isFront) {
            (sensorOrientation - deviceDegrees + FULL_TURN) % FULL_TURN
        } else {
            (sensorOrientation + deviceDegrees) % FULL_TURN
        }

    /** With a lock, only the two ways up in that aspect count, and the last one holds through the others. */
    private fun shownDegrees(): Int = when (aspect) {
        CameraAspect.AUTO -> phoneDegrees
        CameraAspect.LANDSCAPE -> landscapeDegrees
        CameraAspect.PORTRAIT -> portraitDegrees
    }

    private fun sendRotation() {
        val current = config ?: return
        val rotation = rotationFor(shownDegrees()).toUInt()
        if (rotation != current.rotation) {
            val changed = current.copy(rotation = rotation)
            config = changed
            Lis.client.updateVideo(changed)
        }
    }

    private fun turnedTo(degrees: Int) {
        phoneDegrees = degrees
        if (degrees % HALF_TURN == 0) portraitDegrees = degrees else landscapeDegrees = degrees
        sendRotation()
    }

    /** Sensor events arrive on the main thread, so they move over to where the rest of this state lives. */
    private fun watchOrientation(cameraHandler: Handler) {
        val listener = object : OrientationEventListener(context) {
            override fun onOrientationChanged(degrees: Int) {
                if (degrees == ORIENTATION_UNKNOWN) return
                val nearest = (degrees + 45) / 90 * 90 % FULL_TURN
                cameraHandler.post { turnedTo(nearest) }
            }
        }
        orientationListener = listener
        listener.enable()
    }

    private companion object {
        const val TAG = "LisCamera"
        const val FALLBACK_FPS = 30
        const val BITRATE_60 = 16_000_000L
        const val BITRATE_30 = 10_000_000L
        const val MIN_BITRATE = 1_500_000L
        const val MAX_BITRATE = 40_000_000L
        const val PIXELS_1080P = 1920L * 1080L
        const val KEYFRAME_SECONDS = 1
        const val KEYFRAME_REQUEST_MS = 500L
        const val STATS_EVERY_MS = 1000L
        const val NANOS_PER_SECOND = 1_000_000_000L
        const val FRAME_TIME_SLACK_PERCENT = 101L
        const val HALF_TURN = 180
        const val FULL_TURN = 360
        const val STOP_WAIT_SECONDS = 2L
    }
}
