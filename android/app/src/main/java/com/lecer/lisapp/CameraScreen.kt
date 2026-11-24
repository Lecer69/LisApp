package com.lecer.lisapp

import android.util.Size
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import kotlinx.coroutines.delay
import uniffi.lis_ffi.ConnectionState

/** How a photo for the PC is getting on. `done` is the last word, which clears after a moment. */
data class SnapNote(val text: String, val done: Boolean = true, val failed: Boolean = false)

data class CameraStatus(
    val running: Boolean = false,
    val live: Boolean = false,
    val viewers: Int = 0,
    val front: Boolean = false,
    /** What the camera actually offered, as it can be less than asked for. */
    val size: Size? = null,
    val targetFps: Int = 0,
    /** Below the target, dim light can slow the camera down as far as this. */
    val lowestFps: Int = 0,
    val fps: Int = 0,
    val mbps: Float = 0f,
    /** Sending less than chosen, as the connection can't carry more right now. */
    val limited: Boolean = false,
    val error: String? = null,
    /** The camera takes photos beside the stream. */
    val canSnap: Boolean = false,
    val snap: SnapNote? = null,
)

class CameraActions(
    val close: () -> Unit,
    val start: () -> Unit,
    val stop: () -> Unit,
    val switchCamera: () -> Unit,
    val setFps: (Int) -> Unit,
    val setSize: (Size) -> Unit,
    val setAspect: (CameraAspect) -> Unit,
    val snap: () -> Unit,
    val sizes: (front: Boolean) -> List<Size>,
    val surface: (Surface?) -> Unit,
)

// Only for framing, whatever is being sent, so it leaves the camera more room for the stream
private const val PREVIEW_WIDTH = 1280
private const val PREVIEW_HEIGHT = 720
private const val FPS_SLACK = 5
private const val SNAP_NOTE_MS = 4000L

private fun fpsNote(camera: CameraStatus, wantedFps: Int): String = when {
    camera.targetFps in 1 until wantedFps -> " This camera only offers ${camera.targetFps} fps at ${camera.size?.label}."
    camera.lowestFps < camera.targetFps && camera.fps in 1 until camera.targetFps - FPS_SLACK ->
        " This camera slows down in dim light."
    else -> ""
}

private fun statusText(state: AppState, wantedFps: Int): String {
    val camera = state.camera
    val note = fpsNote(camera, wantedFps)
    return when {
        camera.error != null -> camera.error
        !camera.running && state.relayed -> "The camera doesn't go through the relay"
        !camera.running -> if (state.connection == ConnectionState.CONNECTED) "Stopped" else "Connect to your PC first"
        !camera.live -> "Connecting to the PC…"
        camera.viewers == 0 -> "Live. Add the Lis Camera source in OBS.$note"
        else -> {
            val limited = if (camera.limited) " (lowered for the connection)" else ""
            "Live in OBS · ${camera.size?.label} · ${camera.fps} fps · ${"%.1f".format(camera.mbps)} Mbps$limited.$note"
        }
    }
}

@Composable
fun CameraScreen(state: AppState, wantedFps: Int, wantedSize: Size, aspect: CameraAspect, actions: CameraActions) {
    val camera = state.camera
    val connected = state.connection == ConnectionState.CONNECTED
    val sizes = remember(camera.front) { actions.sizes(camera.front) }
    val shape = RoundedCornerShape(12.dp)
    val snap = camera.snap

    LaunchedEffect(snap) {
        if (snap?.done == true) {
            delay(SNAP_NOTE_MS)
            Lis.update { if (this.camera.snap === snap) copy(camera = this.camera.copy(snap = null)) else this }
        }
    }

    ScreenColumn(scroll = false, spacing = 10.dp) {
        ScreenHeader(
            "Camera",
            when {
                camera.running && camera.live -> "Live"
                camera.running -> "Starting…"
                else -> if (camera.front) "Front camera" else "Back camera"
            },
            back = actions.close,
        )

        Box(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .clip(shape)
                .border(1.dp, Theme.border, shape),
            contentAlignment = Alignment.Center,
        ) {
            AndroidView(
                factory = { context ->
                    SurfaceView(context).apply {
                        holder.setFixedSize(PREVIEW_WIDTH, PREVIEW_HEIGHT)
                        holder.addCallback(object : SurfaceHolder.Callback {
                            override fun surfaceCreated(created: SurfaceHolder) = actions.surface(created.surface)

                            override fun surfaceChanged(changed: SurfaceHolder, format: Int, width: Int, height: Int) {}

                            override fun surfaceDestroyed(destroyed: SurfaceHolder) = actions.surface(null)
                        })
                    }
                },
                // The screen stays portrait here, whichever way round the picture is sent
                modifier = Modifier.aspectRatio(PREVIEW_HEIGHT.toFloat() / PREVIEW_WIDTH),
            )
            if (!camera.running) {
                Text("Press Start to show the picture", fontSize = 14.sp, color = Theme.textDim)
            }
        }

        // In the status's place for a moment, so the picture doesn't jump
        if (snap != null) {
            StatusLine(snap.text, error = snap.failed)
        } else {
            StatusLine(statusText(state, wantedFps), error = camera.error != null)
        }

        Column(modifier = Modifier.offset(x = (-12).dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Chips(sizes, closestSize(sizes, wantedSize), { it.label }, enabled = !camera.running, pick = actions.setSize)
            Chips(listOf(60, 30), wantedFps, { "$it fps" }, enabled = !camera.running, pick = actions.setFps)
            Chips(CameraAspect.entries, aspect, { it.label }, pick = actions.setAspect)
        }

        LisButton(
            "Snap to PC",
            actions.snap,
            modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp),
            enabled = camera.running && camera.live && camera.canSnap && snap?.done != false,
            icon = R.drawable.ic_camera,
        )

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp)) {
            if (camera.running) {
                LisButton("Stop", actions.stop, modifier = Modifier.weight(1f).heightIn(min = 48.dp))
            } else {
                LisButton("Start", actions.start, modifier = Modifier.weight(1f).heightIn(min = 48.dp), enabled = connected)
            }
            LisButton(
                if (camera.front) "Back Camera" else "Front Camera",
                actions.switchCamera,
                modifier = Modifier.weight(1f).heightIn(min = 48.dp),
            )
        }
    }
}
