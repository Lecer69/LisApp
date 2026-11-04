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
    private var codecConfig = ByteArray(0)
    private var lastKeyframeRequest = 0L

    private var windowStart = 0L
    private var windowFrames = 0
    private var windowBytes = 0L

    fun start(preview: Surface, front: Boolean, wantedFps: Int) {
        stop()
        val cameraThread = HandlerThread("LisCamera").also { it.start() }
        thread = cameraThread
        handler = Handler(cameraThread.looper).also { cameraHandler ->
            cameraHandler.post { open(preview, front, wantedFps, cameraHandler) }
        }
        Lis.update { copy(camera = CameraStatus(running = true, front = front)) }
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
        Lis.client.stopVideo()
    }

    /** The fastest range at or below what was asked for, with the steadiest frame rate. */
    private fun pickFrameRate(characteristics: CameraCharacteristics, wanted: Int): Range<Int>? {
        val map = characteristics[CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP] ?: return null
        val ranges = characteristics[CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES] ?: return null
        val minFrameTime = map.getOutputMinFrameDuration(MediaCodec::class.java, SIZE)
        return listOf(wanted, FALLBACK_FPS).firstNotNullOfOrNull { fps ->
            if (minFrameTime > NANOS_PER_SECOND / fps) {
                null
            } else {
                ranges.filter { it.upper == fps }.maxByOrNull { it.lower }
            }
        }
    }

    @SuppressLint("MissingPermission")
    private fun open(preview: Surface, front: Boolean, wantedFps: Int, cameraHandler: Handler) {
        try {
            val cameras = manager ?: return fail("This phone has no camera service")
            val facing = if (front) CameraMetadata.LENS_FACING_FRONT else CameraMetadata.LENS_FACING_BACK
            val id = cameras.cameraIdList.firstOrNull {
                cameras.getCameraCharacteristics(it)[CameraCharacteristics.LENS_FACING] == facing
            } ?: return fail("No ${if (front) "front" else "back"} camera")

            val characteristics = cameras.getCameraCharacteristics(id)
            val map = characteristics[CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP]
            if (map?.getOutputSizes(MediaCodec::class.java)?.contains(SIZE) != true) {
                return fail("This camera can't record 1080p")
            }
            val range = pickFrameRate(characteristics, wantedFps) ?: return fail("This camera has no usable frame rate")

            sensorOrientation = characteristics[CameraCharacteristics.SENSOR_ORIENTATION] ?: 0
            isFront = front
            val fps = range.upper
            val bitrate = if (fps > FALLBACK_FPS) BITRATE_60 else BITRATE_30
            val started = VideoConfig(
                width = SIZE.width.toUInt(),
                height = SIZE.height.toUInt(),
                fps = fps.toUInt(),
                bitrate = bitrate.toUInt(),
                rotation = rotationFor(0).toUInt(),
                camera = if (front) "Front" else "Back",
            )
            config = started

            val input = startEncoder(fps, bitrate, cameraHandler)
            encoderInput = input
            Lis.client.startVideo(started)
            watchOrientation()
            Lis.update { copy(camera = camera.copy(targetFps = fps)) }

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

    private fun format(fps: Int, bitrate: Int, tuned: Boolean): MediaFormat =
        MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, SIZE.width, SIZE.height).apply {
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
    private fun startEncoder(fps: Int, bitrate: Int, cameraHandler: Handler): Surface {
        val codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        encoder = codec
        codec.setCallback(encoderCallback, cameraHandler)
        try {
            codec.configure(format(fps, bitrate, tuned = true), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        } catch (error: RuntimeException) {
            Log.w(TAG, "the encoder refused the tuned settings", error)
            codec.reset()
            codec.setCallback(encoderCallback, cameraHandler)
            codec.configure(format(fps, bitrate, tuned = false), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        }
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
        val outputs = listOf(OutputConfiguration(preview), OutputConfiguration(input))
        val callback = object : CameraCaptureSession.StateCallback() {
            override fun onConfigured(configured: CameraCaptureSession) {
                session = configured
                try {
                    val request = opened.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
                        addTarget(preview)
                        addTarget(input)
                        set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, range)
                    }.build()
                    configured.setRepeatingRequest(request, null, cameraHandler)
                } catch (error: CameraAccessException) {
                    fail("Can't start the camera: ${error.message}")
                } catch (error: IllegalStateException) {
                    Log.w(TAG, "the camera closed while starting", error)
                }
            }

            override fun onConfigureFailed(configured: CameraCaptureSession) {
                fail("This camera can't run at ${range.upper} fps here. Try 30 fps.")
            }
        }
        opened.createCaptureSession(
            SessionConfiguration(SessionConfiguration.SESSION_REGULAR, outputs, { cameraHandler.post(it) }, callback),
        )
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
        val needKeyframe = Lis.client.sendVideoFrame(data, info.presentationTimeUs, keyframe)

        val now = SystemClock.elapsedRealtime()
        if (needKeyframe && now - lastKeyframeRequest > KEYFRAME_REQUEST_MS) {
            lastKeyframeRequest = now
            codec.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
        }

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

    /** Degrees clockwise to show the frames upright, as Camera2 documents it. */
    private fun rotationFor(deviceDegrees: Int): Int =
        if (isFront) {
            (sensorOrientation - deviceDegrees + FULL_TURN) % FULL_TURN
        } else {
            (sensorOrientation + deviceDegrees) % FULL_TURN
        }

    private fun watchOrientation() {
        val listener = object : OrientationEventListener(context) {
            override fun onOrientationChanged(degrees: Int) {
                if (degrees == ORIENTATION_UNKNOWN) return
                val rotation = rotationFor((degrees + 45) / 90 * 90 % FULL_TURN).toUInt()
                val current = config ?: return
                if (rotation != current.rotation) {
                    val changed = current.copy(rotation = rotation)
                    config = changed
                    Lis.client.updateVideo(changed)
                }
            }
        }
        orientationListener = listener
        listener.enable()
    }

    private companion object {
        const val TAG = "LisCamera"
        val SIZE = Size(1920, 1080)
        const val FALLBACK_FPS = 30
        const val BITRATE_60 = 16_000_000
        const val BITRATE_30 = 10_000_000
        const val KEYFRAME_SECONDS = 1
        const val KEYFRAME_REQUEST_MS = 500L
        const val STATS_EVERY_MS = 1000L
        const val NANOS_PER_SECOND = 1_000_000_000L
        const val FULL_TURN = 360
        const val STOP_WAIT_SECONDS = 2L
    }
}
