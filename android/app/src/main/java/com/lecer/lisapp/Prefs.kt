package com.lecer.lisapp

import android.content.Context
import android.content.SharedPreferences
import android.util.Size
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.updateAndGet
import org.json.JSONObject

/** The app's own settings. The USB switch keeps its own key, as it was there first. */
data class AppSettings(
    val theme: ThemeMode = ThemeMode.SYSTEM,
    val showPcStats: Boolean = true,
    val showMedia: Boolean = true,
    val showOrganizer: Boolean = true,
    val showActions: Boolean = true,
    val confirmLock: Boolean = false,
    val keepConnected: Boolean = true,
    val startOnBoot: Boolean = true,
    val usbRetrySeconds: Int = 5,
    val statsSeconds: Int = 60,
    val reportCharging: Boolean = true,
    val pauseWithoutWifi: Boolean = true,
    val notifyFiles: Boolean = true,
    val notifyPcLocked: Boolean = true,
    val notifyPcOn: Boolean = true,
    val mediaControls: Boolean = true,
    val volumeStep: Int = 5,
    val unlockPopUp: Boolean = true,
    val ringSeconds: Int = 60,
    val ringFullVolume: Boolean = true,
    val ringVibrate: Boolean = true,
    val cameraHeight: Int = DEFAULT_CAMERA_SIZE.height,
    val cameraFps: Int = 60,
    val cameraAspect: CameraAspect = CameraAspect.AUTO,
    val cameraFront: Boolean = false,
    val weekStartsMonday: Boolean = false,
) {
    val cameraSize: Size get() = CAMERA_SIZES.firstOrNull { it.height == cameraHeight } ?: DEFAULT_CAMERA_SIZE
}

object Prefs {
    private const val FILE = "lis"
    private const val KEY = "settings"

    private var preferences: SharedPreferences? = null
    private val mutable = MutableStateFlow(AppSettings())
    val settings: StateFlow<AppSettings> = mutable
    val current: AppSettings get() = mutable.value

    fun init(context: Context) {
        val opened = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        preferences = opened
        mutable.value = read(opened.getString(KEY, null))
    }

    fun update(change: AppSettings.() -> AppSettings) {
        val next = mutable.updateAndGet { it.change() }
        preferences?.edit()?.putString(KEY, write(next))?.apply()
    }

    private inline fun <reified T : Enum<T>> JSONObject.enum(key: String, fallback: T): T =
        enumValues<T>().firstOrNull { it.name == optString(key) } ?: fallback

    private fun read(text: String?): AppSettings {
        val json = runCatching { JSONObject(text ?: "{}") }.getOrDefault(JSONObject())
        val d = AppSettings()
        return AppSettings(
            theme = json.enum("theme", d.theme),
            showPcStats = json.optBoolean("showPcStats", d.showPcStats),
            showMedia = json.optBoolean("showMedia", d.showMedia),
            showOrganizer = json.optBoolean("showOrganizer", d.showOrganizer),
            showActions = json.optBoolean("showActions", d.showActions),
            confirmLock = json.optBoolean("confirmLock", d.confirmLock),
            keepConnected = json.optBoolean("keepConnected", d.keepConnected),
            startOnBoot = json.optBoolean("startOnBoot", d.startOnBoot),
            usbRetrySeconds = json.optInt("usbRetrySeconds", d.usbRetrySeconds),
            statsSeconds = json.optInt("statsSeconds", d.statsSeconds),
            reportCharging = json.optBoolean("reportCharging", d.reportCharging),
            pauseWithoutWifi = json.optBoolean("pauseWithoutWifi", d.pauseWithoutWifi),
            notifyFiles = json.optBoolean("notifyFiles", d.notifyFiles),
            notifyPcLocked = json.optBoolean("notifyPcLocked", d.notifyPcLocked),
            notifyPcOn = json.optBoolean("notifyPcOn", d.notifyPcOn),
            mediaControls = json.optBoolean("mediaControls", d.mediaControls),
            volumeStep = json.optInt("volumeStep", d.volumeStep),
            unlockPopUp = json.optBoolean("unlockPopUp", d.unlockPopUp),
            ringSeconds = json.optInt("ringSeconds", d.ringSeconds),
            ringFullVolume = json.optBoolean("ringFullVolume", d.ringFullVolume),
            ringVibrate = json.optBoolean("ringVibrate", d.ringVibrate),
            cameraHeight = json.optInt("cameraHeight", d.cameraHeight),
            cameraFps = json.optInt("cameraFps", d.cameraFps),
            cameraAspect = json.enum("cameraAspect", d.cameraAspect),
            cameraFront = json.optBoolean("cameraFront", d.cameraFront),
            weekStartsMonday = json.optBoolean("weekStartsMonday", d.weekStartsMonday),
        )
    }

    private fun write(settings: AppSettings): String = JSONObject()
        .put("theme", settings.theme.name)
        .put("showPcStats", settings.showPcStats)
        .put("showMedia", settings.showMedia)
        .put("showOrganizer", settings.showOrganizer)
        .put("showActions", settings.showActions)
        .put("confirmLock", settings.confirmLock)
        .put("keepConnected", settings.keepConnected)
        .put("startOnBoot", settings.startOnBoot)
        .put("usbRetrySeconds", settings.usbRetrySeconds)
        .put("statsSeconds", settings.statsSeconds)
        .put("reportCharging", settings.reportCharging)
        .put("pauseWithoutWifi", settings.pauseWithoutWifi)
        .put("notifyFiles", settings.notifyFiles)
        .put("notifyPcLocked", settings.notifyPcLocked)
        .put("notifyPcOn", settings.notifyPcOn)
        .put("mediaControls", settings.mediaControls)
        .put("volumeStep", settings.volumeStep)
        .put("unlockPopUp", settings.unlockPopUp)
        .put("ringSeconds", settings.ringSeconds)
        .put("ringFullVolume", settings.ringFullVolume)
        .put("ringVibrate", settings.ringVibrate)
        .put("cameraHeight", settings.cameraHeight)
        .put("cameraFps", settings.cameraFps)
        .put("cameraAspect", settings.cameraAspect.name)
        .put("cameraFront", settings.cameraFront)
        .put("weekStartsMonday", settings.weekStartsMonday)
        .toString()
}
