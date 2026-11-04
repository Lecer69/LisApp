package com.lecer.lisapp

import android.content.Context
import android.provider.Settings
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import uniffi.lis_ffi.ClientListener
import uniffi.lis_ffi.ConnectionState
import uniffi.lis_ffi.LisClient
import uniffi.lis_ffi.PairedPc
import uniffi.lis_ffi.PcStats
import uniffi.lis_ffi.VideoState

data class FoundPc(val name: String, val host: String, val port: Int, val fingerprintStart: String)

data class AppState(
    val connection: ConnectionState = ConnectionState.DISCONNECTED,
    val pcName: String? = null,
    val paired: PairedPc? = null,
    val pairCode: String? = null,
    val pairMessage: String? = null,
    val pcStats: PcStats? = null,
    val found: List<FoundPc> = emptyList(),
    val transfer: String? = null,
    val camera: CameraStatus = CameraStatus(),
)

/** The one client for the process, and the state the screens and the service read. */
object Lis {
    private lateinit var appContext: Context
    private var wantsPairing = false

    /** So a camera the PC turned away stops encoding, on the screen that owns it. */
    var onCameraRefused: (() -> Unit)? = null

    lateinit var client: LisClient
        private set

    private val mutableState = MutableStateFlow(AppState())
    val state: StateFlow<AppState> = mutableState

    fun init(context: Context) {
        appContext = context.applicationContext
        Notifications.createChannels(appContext)
        client = LisClient(appContext.filesDir.absolutePath, deviceName(appContext), Listener())
        refreshPaired()
    }

    private fun deviceName(context: Context): String =
        Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)?.takeIf { it.isNotBlank() } ?: Build.MODEL

    fun update(change: AppState.() -> AppState) {
        mutableState.update { it.change() }
    }

    private fun refreshPaired() {
        val paired = client.pairedPc()
        update { copy(paired = paired) }
    }

    /** Connects and asks to pair as soon as the PC answers. */
    fun pairWith(host: String, port: Int) {
        wantsPairing = true
        update { copy(pairMessage = null, pairCode = null) }
        client.connect(host, port.toUShort())
        if (state.value.connection == ConnectionState.UNPAIRED) startPairing()
    }

    private fun startPairing() {
        wantsPairing = false
        client.startPairing()
    }

    fun answerCode(accept: Boolean) {
        client.confirmPairing(accept)
        update { copy(pairCode = null) }
    }

    fun unpair() {
        client.unpair()
        refreshPaired()
        update { copy(pcStats = null, pcName = null) }
    }

    private class Listener : ClientListener {
        override fun onState(state: ConnectionState, pcName: String?) {
            update { copy(connection = state, pcName = pcName ?: this.pcName) }
            refreshPaired()
            if (state == ConnectionState.UNPAIRED && wantsPairing) startPairing()
            if (state == ConnectionState.CONNECTED) StatsReporter.sendNow(appContext)
        }

        override fun onPairCode(code: String) {
            update { copy(pairCode = code, pairMessage = null) }
        }

        override fun onPairResult(ok: Boolean, reason: String) {
            update { copy(pairCode = null, pairMessage = if (ok) null else reason) }
            refreshPaired()
        }

        override fun onPcStats(stats: PcStats) {
            update { copy(pcStats = stats) }
        }

        override fun openIncomingFile(id: ULong, name: String, size: ULong): Int =
            Transfers.openIncoming(appContext, id, name)

        override fun onFileProgress(id: ULong, name: String, incoming: Boolean, done: ULong, total: ULong) {
            val percent = if (total > 0u) (done.toDouble() * 100 / total.toDouble()).toInt() else 0
            update { copy(transfer = "${if (incoming) "Receiving" else "Sending"} $name: $percent%") }
        }

        override fun onFileFinished(id: ULong, name: String, incoming: Boolean, ok: Boolean) {
            Transfers.finish(appContext, id, name, incoming, ok)
        }

        override fun onVideoState(state: VideoState) {
            when (state) {
                VideoState.Connecting -> update { copy(camera = camera.copy(live = false)) }
                VideoState.Live -> update { copy(camera = camera.copy(live = true, error = null)) }
                is VideoState.Refused -> {
                    update { copy(camera = camera.copy(live = false, error = state.reason)) }
                    onCameraRefused?.invoke()
                }
            }
        }

        override fun onVideoViewers(count: UInt) {
            update { copy(camera = camera.copy(viewers = count.toInt())) }
        }
    }
}
