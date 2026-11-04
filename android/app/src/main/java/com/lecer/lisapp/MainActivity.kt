package com.lecer.lisapp

import android.Manifest
import android.annotation.SuppressLint
import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.ComponentActivity
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
                LisScreen(
                    state = state,
                    setup = setup,
                    actions = ScreenActions(
                        pair = { host, port -> Lis.pairWith(host, port) },
                        answerCode = Lis::answerCode,
                        lockPc = { Lis.client.lockPc() },
                        sendFile = { pickFiles.launch(arrayOf("*/*")) },
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

    /** Without a paired PC there's nothing to stay connected for. */
    override fun onStop() {
        super.onStop()
        if (Lis.client.pairedPc() == null && !isChangingConfigurations) {
            Lis.client.disconnect()
            SyncService.stop(this)
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
}
