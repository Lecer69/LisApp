package com.lecer.lisapp

import android.util.Size
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface as Material
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import uniffi.lis_ffi.ConnectionState

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
    val error: String? = null,
)

class CameraActions(
    val close: () -> Unit,
    val start: () -> Unit,
    val stop: () -> Unit,
    val switchCamera: () -> Unit,
    val setFps: (Int) -> Unit,
    val setSize: (Size) -> Unit,
    val setAspect: (CameraAspect) -> Unit,
    val sizes: (front: Boolean) -> List<Size>,
    val surface: (Surface?) -> Unit,
)

// Only for framing, whatever is being sent, so it leaves the camera more room for the stream
private const val PREVIEW_WIDTH = 1280
private const val PREVIEW_HEIGHT = 720
private const val FPS_SLACK = 5

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
        !camera.running -> if (state.connection == ConnectionState.CONNECTED) "Stopped" else "Connect to your PC first"
        !camera.live -> "Connecting to the PC…"
        camera.viewers == 0 -> "Live. Add the Lis Camera source in OBS.$note"
        else -> "Live in OBS · ${camera.size?.label} · ${camera.fps} fps · ${"%.1f".format(camera.mbps)} Mbps.$note"
    }
}

/** Scrolls sideways, as a phone's width doesn't always fit them all. */
@Composable
private fun <T> ChoiceRow(options: List<T>, selected: T?, enabled: Boolean, label: (T) -> String, pick: (T) -> Unit) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
    ) {
        options.forEach { option ->
            FilterChip(
                selected = option == selected,
                enabled = enabled,
                onClick = { pick(option) },
                label = { Text(label(option)) },
            )
        }
    }
}

@Composable
fun CameraScreen(state: AppState, wantedFps: Int, wantedSize: Size, aspect: CameraAspect, actions: CameraActions) {
    val camera = state.camera
    val connected = state.connection == ConnectionState.CONNECTED
    val sizes = remember(camera.front) { actions.sizes(camera.front) }

    Material(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(
            modifier = Modifier.fillMaxSize().safeDrawingPadding().padding(horizontal = 20.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text("Camera", fontSize = 24.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                TextButton(onClick = actions.close) { Text("Close") }
            }

            Box(modifier = Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
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
            }

            Text(
                statusText(state, wantedFps),
                fontSize = 14.sp,
                color = if (camera.error != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
            )

            ChoiceRow(sizes, closestSize(sizes, wantedSize), !camera.running, { it.label }, actions.setSize)
            ChoiceRow(listOf(60, 30), wantedFps, !camera.running, { "$it fps" }, actions.setFps)
            ChoiceRow(CameraAspect.entries, aspect, true, { it.label }, actions.setAspect)

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                if (camera.running) {
                    Button(onClick = actions.stop, modifier = Modifier.weight(1f)) { Text("Stop") }
                } else {
                    Button(enabled = connected, onClick = actions.start, modifier = Modifier.weight(1f)) { Text("Start") }
                }
                OutlinedButton(onClick = actions.switchCamera, modifier = Modifier.weight(1f)) {
                    Text(if (camera.front) "Use back camera" else "Use front camera")
                }
            }
        }
    }
}
