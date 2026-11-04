package com.lecer.lisapp

import android.Manifest
import android.annotation.SuppressLint
import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.view.Surface
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.content.ContextCompat
import androidx.core.content.IntentCompat
import androidx.core.content.getSystemService
import androidx.core.net.toUri

class MainActivity : ComponentActivity() {
    private var setup by mutableStateOf(SetupState())
    private var showCamera by mutableStateOf(false)
    private var wantedFps by mutableStateOf(DEFAULT_FPS)
    private var wantedSize by mutableStateOf(DEFAULT_CAMERA_SIZE)
    private var aspect by mutableStateOf(CameraAspect.AUTO)

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
        uris.forEach { Transfers.send(this, it) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            val state by Lis.state.collectAsState()
            LisTheme {
                if (showCamera) {
                    BackHandler(onBack = ::closeCamera)
                    CameraScreen(
                        state = state,
                        wantedFps = wantedFps,
                        wantedSize = wantedSize,
                        aspect = aspect,
                        actions = CameraActions(
                            close = ::closeCamera,
                            start = ::requestCamera,
                            stop = ::stopCamera,
                            switchCamera = ::switchCamera,
                            setFps = { wantedFps = it },
                            setSize = { wantedSize = it },
                            setAspect = {
                                aspect = it
                                camera.setAspect(it)
                            },
                            sizes = camera::sizes,
                            surface = ::onPreviewSurface,
                        ),
                    )
                    return@LisTheme
                }
                LisScreen(
                    state = state,
                    setup = setup,
                    actions = ScreenActions(
                        pair = { host, port -> Lis.pairWith(host, port) },
                        answerCode = Lis::answerCode,
                        lockPc = { Lis.client.lockPc() },
                        sendFile = { pickFiles.launch(arrayOf("*/*")) },
                        openCamera = ::openCamera,
                        unpair = {
                            Lis.unpair()
                            refreshSetup()
                        },
                        askNotifications = ::askNotifications,
                        askBackground = ::askBackground,
                        openAutostart = ::openAutostart,
                    ),
                )
            }
        }
        handleShare(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleShare(intent)
    }

    override fun onStart() {
        super.onStart()
        refreshSetup()
        SyncService.start(this)
    }

    /** The camera never runs off screen. Without a paired PC, there's nothing to stay connected for. */
    override fun onStop() {
        stopCamera()
        super.onStop()
        if (Lis.client.pairedPc() == null && !isChangingConfigurations) {
            Lis.client.disconnect()
            SyncService.stop(this)
        }
    }

    @SuppressLint("SourceLockedOrientationActivity")
    private fun openCamera() {
        Lis.onCameraRefused = { runOnUiThread(::stopCamera) }
        Lis.update { copy(camera = camera.copy(error = null)) }
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        showCamera = true
    }

    private fun closeCamera() {
        stopCamera()
        showCamera = false
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
        camera.start(surface, front, wantedSize, wantedFps)
    }

    private fun stopCamera() {
        camera.stop()
        window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    }

    private fun switchCamera() {
        val status = Lis.state.value.camera
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
        )
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
        const val DEFAULT_FPS = 60
    }
}
