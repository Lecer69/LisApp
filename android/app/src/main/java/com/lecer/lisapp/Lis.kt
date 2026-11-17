package com.lecer.lisapp

import android.content.Context
import android.provider.Settings
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import uniffi.lis_ffi.ActionButton
import uniffi.lis_ffi.ClientListener
import uniffi.lis_ffi.ConnectionState
import uniffi.lis_ffi.LisClient
import uniffi.lis_ffi.MediaInfo
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
    val ringing: Boolean = false,
    val usbMode: Boolean = false,
    val wifiOff: Boolean = false,
    /** Wi-Fi is off, so nothing looks for the PC, to save battery. The cable still works. */
    val paused: Boolean = false,
    val media: MediaInfo? = null,
    val actions: List<ActionButton> = emptyList(),
    val actionMessage: String? = null,
    val syncError: String? = null,
    val pcLocked: Boolean = false,
    val unlockAvailable: Boolean = false,
    /** This phone has made its unlock key. Whether the PC approved it shows in `unlockAvailable`. */
    val unlockKey: Boolean = false,
    val unlockMessage: String? = null,
)

/** The one client for the process, and the state the screens and the service read. */
object Lis {
    private const val PREFERENCES = "lis"
    private const val PREFERENCE_USB = "usb_mode"
    /** Where `adb reverse` puts the PC on this phone. */
    private const val USB_HOST = "127.0.0.1"

    private lateinit var appContext: Context
    private var wantsPairing = false

    /** So a camera the PC turned away stops encoding, on the screen that owns it. */
    var onCameraRefused: (() -> Unit)? = null

    /** Take Photo in the shell, for the screen that holds the camera. */
    var onSnapRequest: (() -> Unit)? = null

    /** The PC's challenge reaches the screen that asked for it, which holds the fingerprint prompt. */
    var onUnlockChallenge: ((String) -> Unit)? = null

    /** That screen can close, with what to tell the person, if anything. */
    var onUnlockOver: ((String?) -> Unit)? = null

    lateinit var client: LisClient
        private set

    private val mutableState = MutableStateFlow(AppState())
    val state: StateFlow<AppState> = mutableState

    fun init(context: Context) {
        appContext = context.applicationContext
        Prefs.init(appContext)
        EventLog.init(appContext)
        Notifications.createChannels(appContext)
        client = LisClient(appContext.filesDir.absolutePath, deviceName(appContext), Listener())
        val usb = appContext.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getBoolean(PREFERENCE_USB, false)
        update { copy(usbMode = usb, unlockKey = Unlocker.hasKey(), camera = camera.copy(front = Prefs.current.cameraFront)) }
        refreshPaired()
        SyncDocs.reloadAll()
    }

    /** To the paired PC, over the cable or the network. */
    fun connectToPc() {
        if (state.value.paused) return
        val pc = client.pairedPc() ?: return
        client.connect(if (state.value.usbMode) USB_HOST else pc.host, pc.port)
    }

    fun setUsbMode(on: Boolean) {
        appContext.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putBoolean(PREFERENCE_USB, on).apply()
        update { copy(usbMode = on) }
        refreshPause()
        connectToPc()
    }

    fun setWifi(on: Boolean) {
        update { copy(wifiOff = !on) }
        refreshPause()
    }

    fun refreshPause() {
        val current = state.value
        val paused = !current.usbMode && current.wifiOff && Prefs.current.pauseWithoutWifi
        if (paused == current.paused) return
        update { copy(paused = paused) }
        if (paused) client.disconnect() else connectToPc()
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
        SyncDocs.clear()
    }

    private const val PREFERENCE_PC_BOOT = "pc_boot_at"
    /** Only a PC up for less than this has just turned on. */
    private const val BOOT_WINDOW_SECS = 15 * 60L
    /** Boot times from two readings of the uptime differ by a little. */
    private const val SAME_BOOT_MS = 2 * 60_000L

    /** The first figures of each connection tell whether the PC booted since it was last seen. */
    private var checkedBoot = false

    private fun pcLabel(): String = state.value.pcName ?: state.value.paired?.name ?: "the PC"

    private fun noteUptime(uptime: ULong) {
        if (checkedBoot) return
        checkedBoot = true
        val preferences = appContext.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val bootAt = System.currentTimeMillis() - uptime.toLong() * 1000
        val known = preferences.getLong(PREFERENCE_PC_BOOT, 0L)
        if (kotlin.math.abs(bootAt - known) < SAME_BOOT_MS) return
        preferences.edit().putLong(PREFERENCE_PC_BOOT, bootAt).apply()
        if (uptime.toLong() > BOOT_WINDOW_SECS) return
        EventLog.add(EventKind.PC_ON, "${pcLabel()} turned on")
        Notifications.showPcOn(appContext, pcLabel())
    }

    private class Listener : ClientListener {
        override fun onState(state: ConnectionState, pcName: String?) {
            val was = Lis.state.value.connection
            update { copy(connection = state, pcName = pcName ?: this.pcName) }
            refreshPaired()
            if (state == ConnectionState.UNPAIRED && wantsPairing) startPairing()
            if (state == ConnectionState.CONNECTED) {
                checkedBoot = false
                EventLog.add(EventKind.CONNECTED, "Connected to ${pcLabel()}")
                StatsReporter.sendNow(appContext)
            } else {
                if (was == ConnectionState.CONNECTED) EventLog.add(EventKind.DISCONNECTED, "Disconnected from ${pcLabel()}")
                update { copy(media = null, actions = emptyList(), pcLocked = false, unlockAvailable = false) }
                MediaRemote.clear(appContext)
                Notifications.cancelPcLocked(appContext)
                Notifications.cancelUnlockPrompt(appContext)
                onUnlockOver?.invoke(null)
            }
        }

        override fun onPairCode(code: String) {
            update { copy(pairCode = code, pairMessage = null) }
        }

        override fun onPairResult(ok: Boolean, reason: String) {
            update { copy(pairCode = null, pairMessage = if (ok) null else reason) }
            refreshPaired()
        }

        override fun onPcStats(stats: PcStats) {
            noteUptime(stats.uptimeSecs)
            update { copy(pcStats = stats) }
        }

        override fun openIncomingFile(id: ULong, name: String, size: ULong): Int {
            Transfers.noteIncomingSize(id, size.toLong())
            return Transfers.openIncoming(appContext, id, name)
        }

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

        override fun onRing(ring: Boolean) {
            if (ring) Ringer.start(appContext) else Ringer.stop(appContext)
        }

        override fun onSyncChanged(kind: String) {
            update { copy(syncError = null) }
            SyncDocs.reload(kind)
        }

        override fun onSyncError(kind: String, reason: String) {
            update { copy(syncError = reason) }
        }

        override fun onMedia(media: MediaInfo?) {
            update { copy(media = media) }
            MediaRemote.update(appContext, media)
        }

        override fun onMediaArt(data: ByteArray) {
            MediaRemote.updateArt(appContext, data)
        }

        override fun onActions(actions: List<ActionButton>) {
            update { copy(actions = actions) }
        }

        override fun onActionResult(id: String, ok: Boolean) {
            val name = state.value.actions.firstOrNull { it.id == id }?.name ?: "That action"
            update { copy(actionMessage = if (ok) "$name is done" else "$name didn't work") }
        }

        override fun onUnlockState(locked: Boolean, available: Boolean) {
            update { copy(pcLocked = locked, unlockAvailable = available) }
            if (locked && available) {
                Notifications.showPcLocked(appContext, state.value.paired?.name ?: "Your PC")
            } else {
                Notifications.cancelPcLocked(appContext)
                Notifications.cancelUnlockPrompt(appContext)
                onUnlockOver?.invoke(null)
            }
        }

        override fun onUnlockPrompt() {
            UnlockActivity.popUp(appContext)
        }

        override fun onUnlockChallenge(nonce: String) {
            onUnlockChallenge?.invoke(nonce)
        }

        override fun onUnlockResult(ok: Boolean, reason: String) {
            update { copy(unlockMessage = if (ok) "Unlocked" else reason) }
            onUnlockOver?.invoke(if (ok) null else reason)
        }

        override fun onSnapRequest() {
            onSnapRequest?.invoke()
        }
    }
}
