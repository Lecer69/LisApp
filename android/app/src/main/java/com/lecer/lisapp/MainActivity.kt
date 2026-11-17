package com.lecer.lisapp

import android.Manifest
import android.annotation.SuppressLint
import android.app.NotificationManager
import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.view.Surface
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.content.ContextCompat
import androidx.core.content.IntentCompat
import androidx.core.content.getSystemService
import androidx.core.net.toUri
import java.security.GeneralSecurityException

class MainActivity : ComponentActivity() {
    private var setup by mutableStateOf(SetupState())
    private var screen by mutableStateOf<Screen>(Screen.Home)
    /** The file chooser covers the app, which isn't leaving it. */
    private var pickingFiles = false

    private val camera by lazy { CameraStreamer(applicationContext) }
    private var previewSurface: Surface? = null

    private val askCamera = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            startCamera()
        } else {
            Lis.update { copy(camera = camera.copy(error = "Lis App needs camera access for this")) }
        }
    }

    private val askPermissions = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
        refreshSetup()
    }

    private val pickFiles = registerForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        pickingFiles = false
        uris.forEach { Transfers.send(this, it) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val screenActions = ScreenActions(
            pair = { host, port -> Lis.pairWith(host, port) },
            answerCode = Lis::answerCode,
            lockPc = { Lis.client.lockPc() },
            sendFile = ::sendFile,
            openCamera = ::openCamera,
            openPage = { screen = Screen.Organizer(it) },
            openSettings = { screen = Screen.Settings },
            openLogs = { screen = Screen.Logs },
            stopRinging = { Ringer.stop(this) },
            mediaCommand = { Lis.client.mediaCommand(it) },
            setVolume = { Lis.client.mediaSetVolume(it.toUByte()) },
            runAction = { Lis.client.runAction(it) },
            unlockPc = ::unlockPc,
        )
        val settingsActions = SettingsActions(
            back = { screen = Screen.Home },
            setUsb = Lis::setUsbMode,
            reconnect = Lis::connectToPc,
            turnOnUnlock = ::turnOnUnlock,
            turnOffUnlock = ::turnOffUnlock,
            allowPopUp = ::allowPopUp,
            openHyperOsPermissions = ::openHyperOsPermissions,
            askNotifications = ::askNotifications,
            openNotificationSettings = ::openNotificationSettings,
            askBackground = ::askBackground,
            openAutostart = ::openAutostart,
            openAppSettings = ::openAppSettings,
            refreshMedia = { MediaRemote.refresh(applicationContext) },
            unpair = {
                Lis.unpair()
                refreshSetup()
                screen = Screen.Home
            },
        )
        val cameraActions = CameraActions(
            close = ::closeCamera,
            start = ::requestCamera,
            stop = ::stopCamera,
            switchCamera = ::switchCamera,
            setFps = { fps -> Prefs.update { copy(cameraFps = fps) } },
            setSize = { size -> Prefs.update { copy(cameraHeight = size.height) } },
            setAspect = { wanted ->
                Prefs.update { copy(cameraAspect = wanted) }
                camera.setAspect(wanted)
            },
            snap = ::snap,
            sizes = camera::sizes,
            surface = ::onPreviewSurface,
        )

        setContent {
            val state by Lis.state.collectAsState()
            val settings by Prefs.settings.collectAsState()
            val light = settings.theme.light(isSystemInDarkTheme())
            LaunchedEffect(light) { styleWindow(light) }

            LisTheme(light) {
                AnimatedContent(
                    targetState = screen,
                    transitionSpec = { fadeIn(tween(180)) togetherWith fadeOut(tween(120)) },
                    label = "screen",
                ) { shown ->
                    when (shown) {
                        Screen.Camera -> {
                            BackHandler(onBack = ::closeCamera)
                            CameraScreen(
                                state = state,
                                wantedFps = settings.cameraFps,
                                wantedSize = settings.cameraSize,
                                aspect = settings.cameraAspect,
                                actions = cameraActions,
                            )
                        }
                        is Screen.Organizer -> {
                            BackHandler(onBack = ::closePage)
                            OrganizerScreen(shown.page, ::closePage)
                        }
                        Screen.Logs -> {
                            BackHandler { screen = Screen.Home }
                            LogsScreen { screen = Screen.Home }
                        }
                        Screen.Settings -> {
                            BackHandler { screen = Screen.Home }
                            SettingsScreen(state, settings, setup, settingsActions)
                        }
                        Screen.Home -> LisScreen(state, settings, setup, screenActions)
                    }
                }
            }
        }
        handleShare(intent)
    }

    /** The bars' icons and the window behind the app follow the app's theme, which can differ from the system's. */
    private fun styleWindow(light: Boolean) {
        val bars = if (light) SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT) else SystemBarStyle.dark(Color.TRANSPARENT)
        enableEdgeToEdge(statusBarStyle = bars, navigationBarStyle = bars)
        window.decorView.setBackgroundColor(if (light) LIGHT_WINDOW else DARK_WINDOW)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleShare(intent)
    }

    override fun onStart() {
        super.onStart()
        SyncService.start(this)
        SyncDocs.checkDay()
    }

    /** Also on coming back from a system settings page, some of which only pause the app. */
    override fun onResume() {
        super.onResume()
        refreshSetup()
    }

    private fun closePage() {
        SyncDocs.flushAll()
        screen = Screen.Home
    }

    private fun sendFile() {
        pickingFiles = true
        pickFiles.launch(arrayOf("*/*"))
    }

    private fun unlockPc() {
        startActivity(UnlockActivity.intent(this))
    }

    private fun turnOnUnlock() {
        val problem = Unlocker.problem(this)
        if (problem != null) {
            Lis.update { copy(unlockMessage = problem) }
            return
        }
        try {
            Lis.client.unlockEnroll(Unlocker.publicKey())
            Lis.update { copy(unlockKey = true, unlockMessage = "Now approve this phone on the PC: Settings, Phone Sync") }
        } catch (error: GeneralSecurityException) {
            Lis.update { copy(unlockMessage = "Can't make the key: ${error.message}") }
        }
    }

    private fun turnOffUnlock() {
        Unlocker.deleteKey()
        Lis.client.unlockRevoke()
        Lis.update { copy(unlockKey = false, unlockMessage = null) }
    }

    /**
     * The camera never runs off screen. Without a paired PC, there's nothing to stay connected for,
     * and the connection can be set to last only while the app is open.
     */
    override fun onStop() {
        stopCamera()
        SyncDocs.flushAll()
        super.onStop()
        val leaving = Lis.client.pairedPc() == null || !Prefs.current.keepConnected
        if (leaving && !isChangingConfigurations && !pickingFiles) {
            Lis.client.disconnect()
            SyncService.stop(this)
        }
    }

    @SuppressLint("SourceLockedOrientationActivity")
    private fun openCamera() {
        Lis.onCameraRefused = { runOnUiThread(::stopCamera) }
        Lis.onSnapRequest = { runOnUiThread(::snap) }
        val front = Prefs.current.cameraFront
        Lis.update { copy(camera = camera.copy(error = null, front = if (camera.running) camera.front else front)) }
        camera.setAspect(Prefs.current.cameraAspect)
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        screen = Screen.Camera
    }

    private fun closeCamera() {
        stopCamera()
        screen = Screen.Home
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
    }

    private fun onPreviewSurface(surface: Surface?) {
        previewSurface = surface
        if (surface == null) stopCamera()
    }

    private fun requestCamera() {
        if (granted(Manifest.permission.CAMERA)) {
            startCamera()
        } else {
            askCamera.launch(Manifest.permission.CAMERA)
        }
    }

    private fun startCamera(front: Boolean = Lis.state.value.camera.front) {
        val surface = previewSurface ?: return
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        val settings = Prefs.current
        camera.start(surface, front, settings.cameraSize, settings.cameraFps)
    }

    private fun stopCamera() {
        camera.stop()
        window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    }

    private fun snap() {
        val status = Lis.state.value.camera
        if (!status.running || !status.canSnap || status.snap?.done == false) return
        Lis.update { copy(camera = camera.copy(snap = SnapNote("Taking the photo…", done = false))) }
        val taking = camera.snap { photo ->
            if (photo != null) {
                Transfers.sendSnap(applicationContext, photo)
            } else {
                Lis.update { copy(camera = camera.copy(snap = SnapNote("Couldn't take the photo", failed = true))) }
            }
        }
        if (!taking) Lis.update { copy(camera = camera.copy(snap = null)) }
    }

    private fun switchCamera() {
        val status = Lis.state.value.camera
        Prefs.update { copy(cameraFront = !status.front) }
        if (status.running) {
            startCamera(front = !status.front)
        } else {
            Lis.update { copy(camera = camera.copy(front = !status.front)) }
        }
    }

    private fun handleShare(intent: Intent?) {
        val uris: List<Uri> = when (intent?.action) {
            Intent.ACTION_SEND -> listOfNotNull(IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java))
            Intent.ACTION_SEND_MULTIPLE -> IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java).orEmpty()
            else -> emptyList()
        }
        uris.forEach { Transfers.send(this, it) }
        // Not again on rotation
        intent?.action = null
    }

    private fun granted(permission: String): Boolean =
        ContextCompat.checkSelfPermission(this, permission) == PackageManager.PERMISSION_GRANTED

    private fun refreshSetup() {
        setup = SetupState(
            notifications = granted(Manifest.permission.POST_NOTIFICATIONS),
            background = getSystemService<PowerManager>()?.isIgnoringBatteryOptimizations(packageName) == true,
            popUp = canOpenFullScreen() && Settings.canDrawOverlays(this),
            hyperOs = Build.MANUFACTURER.equals("Xiaomi", ignoreCase = true),
        )
    }

    private fun canOpenFullScreen(): Boolean =
        Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE ||
            getSystemService<NotificationManager>()?.canUseFullScreenIntent() == true

    /** One at a time, as these are separate pages of Android's settings. */
    private fun allowPopUp() {
        val page = if (canOpenFullScreen()) Settings.ACTION_MANAGE_OVERLAY_PERMISSION else Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT
        if (!tryOpen(Intent(page, "package:$packageName".toUri()))) openAppSettings()
    }

    /** HyperOS's own list, with its "pop-up windows while in the background" and "lock screen" permissions. */
    private fun openHyperOsPermissions() {
        val editor = Intent("miui.intent.action.APP_PERM_EDITOR")
            .setClassName("com.miui.securitycenter", "com.miui.permcenter.permissions.PermissionsEditorActivity")
            .putExtra("extra_pkgname", packageName)
        if (!tryOpen(editor)) openAppSettings()
    }

    private fun askNotifications() {
        askPermissions.launch(arrayOf(Manifest.permission.POST_NOTIFICATIONS))
    }

    @SuppressLint("BatteryLife")
    private fun askBackground() {
        open(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, "package:$packageName".toUri()))
    }

    /** HyperOS's own list of apps that may start by themselves. */
    private fun openAutostart() {
        val autostart = Intent().setComponent(
            ComponentName("com.miui.securitycenter", "com.miui.permcenter.autostart.AutoStartManagementActivity"),
        )
        if (!tryOpen(autostart)) openAppSettings()
    }

    private fun openNotificationSettings() {
        val page = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, packageName)
        if (!tryOpen(page)) openAppSettings()
    }

    private fun openAppSettings() {
        open(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, "package:$packageName".toUri()))
    }

    private fun open(intent: Intent) {
        tryOpen(intent)
    }

    private fun tryOpen(intent: Intent): Boolean = try {
        startActivity(intent)
        true
    } catch (_: ActivityNotFoundException) {
        false
    } catch (_: SecurityException) {
        false
    }

    private companion object {
        val DARK_WINDOW = 0xFF0F0F0F.toInt()
        val LIGHT_WINDOW = 0xFFF5F5F5.toInt()
    }
}
