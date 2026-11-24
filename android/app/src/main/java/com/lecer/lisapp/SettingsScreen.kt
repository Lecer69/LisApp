package com.lecer.lisapp

import android.content.pm.PackageManager
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

class SettingsActions(
    val back: () -> Unit,
    val setUsb: (Boolean) -> Unit,
    val reconnect: () -> Unit,
    val turnOnUnlock: () -> Unit,
    val turnOffUnlock: () -> Unit,
    val allowPopUp: () -> Unit,
    val openHyperOsPermissions: () -> Unit,
    val askNotifications: () -> Unit,
    val openNotificationSettings: () -> Unit,
    val askBackground: () -> Unit,
    val openAutostart: () -> Unit,
    val openAppSettings: () -> Unit,
    val refreshMedia: () -> Unit,
    val unpair: () -> Unit,
)

private fun seconds(value: Int): String = if (value < 60) "$value s" else "${value / 60} min"

/** The first part of a fingerprint, in groups, enough to compare by eye. */
private fun shortFingerprint(fingerprint: String): String =
    fingerprint.filter { it.isLetterOrDigit() }.take(16).chunked(4).joinToString(" ")

private fun set(change: AppSettings.() -> AppSettings) = Prefs.update(change)

@Composable
private fun Allowed(allowed: Boolean, action: String = "Allow", onClick: () -> Unit) {
    if (allowed) {
        Text("Allowed", fontSize = 14.sp, color = Theme.value)
    } else {
        LisButton(action, onClick)
    }
}

@Composable
private fun Forward() {
    Glyph(R.drawable.ic_forward, size = 18.dp, tint = Theme.faint)
}

@Composable
fun SettingsScreen(state: AppState, settings: AppSettings, setup: SetupState, actions: SettingsActions) {
    val context = LocalContext.current
    val version = remember {
        runCatching { context.packageManager.getPackageInfo(context.packageName, PackageManager.PackageInfoFlags.of(0)).versionName }
            .getOrNull() ?: ""
    }
    val phoneFingerprint = remember { runCatching { Lis.client.fingerprint() }.getOrDefault("") }
    val pc = state.paired
    var unpairing by remember { mutableStateOf(false) }
    var stoppingUnlock by remember { mutableStateOf(false) }

    ScreenColumn(spacing = 0.dp) {
        ScreenHeader("Settings", pc?.name, back = actions.back)

        SectionHeader("Appearance", top = 4.dp)
        ChoiceRow("Theme", "The shell's colors, dark or light", ThemeMode.entries, settings.theme, { it.label }) {
            set { copy(theme = it) }
        }

        SectionHeader("Home")
        ToggleRow("PC figures", "CPU, memory and how long it's been up", settings.showPcStats) { set { copy(showPcStats = it) } }
        ToggleRow("What's playing", "The PC's media, with its controls and volume", settings.showMedia) { set { copy(showMedia = it) } }
        ToggleRow("Organizer", "Todos, notes, goals and the calendar", settings.showOrganizer) { set { copy(showOrganizer = it) } }
        ToggleRow("Action buttons", "The ones set up in the PC's Phone Sync settings", settings.showActions) { set { copy(showActions = it) } }
        ToggleRow("Ask before locking the PC", null, settings.confirmLock) { set { copy(confirmLock = it) } }

        if (pc != null) {
            SectionHeader("Connection")
            ToggleRow(
                "Use the USB cable",
                "Steadier for the camera, and charges the phone. Needs USB debugging on, and USB connection on in the PC's Phone Sync settings.",
                state.usbMode,
                onChange = actions.setUsb,
            )
            if (state.usbMode) {
                ChoiceRow("Look for the cable every", null, listOf(2, 5, 10, 30), settings.usbRetrySeconds, ::seconds) {
                    set { copy(usbRetrySeconds = it) }
                }
            } else {
                ToggleRow(
                    "Pause while Wi-Fi is off",
                    if (state.hasRelay) "Not while the PC has a relay server: it goes through that instead" else "Saves battery. Turn it off if the PC uses this phone's hotspot.",
                    settings.pauseWithoutWifi,
                ) { set { copy(pauseWithoutWifi = it) } }
            }
            ToggleRow(
                "Stay connected in the background",
                "Off, the connection ends when you leave the app, so the PC can't ring the phone, ask for your fingerprint or show its media.",
                settings.keepConnected,
            ) { set { copy(keepConnected = it) } }
            ToggleRow(
                "Connect when the phone starts",
                null,
                settings.startOnBoot && settings.keepConnected,
                enabled = settings.keepConnected,
            ) { set { copy(startOnBoot = it) } }
            SettingRow(
                "Relay server",
                Lis.client.relay()?.let { "${it.host}:${it.port}. For when the PC can't be reached here, set in the PC's Phone Sync settings. No files or camera through it." }
                    ?: "None. Set one in the PC's Phone Sync settings to reach it away from home.",
            )
            SettingRow("Reconnect now", shortStatus(state), onClick = actions.reconnect) {
                LisButton("Reconnect", actions.reconnect)
            }
        }

        SectionHeader("Phone Figures")
        ChoiceRow(
            "Send them every",
            "Battery, storage, memory and network, for the PC's Phone panel and its battery alerts",
            listOf(15, 30, 60, 120, 300),
            settings.statsSeconds,
            ::seconds,
        ) { set { copy(statsSeconds = it) } }
        ToggleRow("Send charging changes at once", "When the charger goes in or comes out", settings.reportCharging) {
            set { copy(reportCharging = it) }
        }

        SectionHeader("Notifications")
        SettingRow("Notifications", "For the connection, transfers and unlocking") {
            Allowed(setup.notifications, onClick = actions.askNotifications)
        }
        ToggleRow("Finished transfers", "When a file is sent, or saved to Downloads", settings.notifyFiles) {
            set { copy(notifyFiles = it) }
        }
        ToggleRow("PC is locked", "Tap it to unlock with your fingerprint", settings.notifyPcLocked) {
            set { copy(notifyPcLocked = it) }
        }
        ToggleRow("PC turned on", "When it connects after a boot", settings.notifyPcOn) {
            set { copy(notifyPcOn = it) }
        }
        SettingRow("Sounds and categories", "Android's own settings for each kind", onClick = actions.openNotificationSettings) {
            Forward()
        }

        SectionHeader("Media")
        ToggleRow(
            "PC media on the lock screen",
            "While it shows, the volume buttons turn the PC's volume",
            settings.mediaControls,
        ) {
            set { copy(mediaControls = it) }
            actions.refreshMedia()
        }
        ChoiceRow("Volume step", "For the volume buttons and the lock screen's", listOf(2, 5, 10), settings.volumeStep, { "$it%" }) {
            set { copy(volumeStep = it) }
        }

        if (pc != null) {
            SectionHeader("Fingerprint Unlock")
            ToggleRow(
                "Unlock the PC with your fingerprint",
                state.unlockMessage ?: if (state.unlockKey) {
                    "If it isn't offered when the PC locks, approve this phone in the PC's Phone Sync settings"
                } else {
                    "A key in this phone's secure hardware signs only after your fingerprint. You approve it on the PC once."
                },
                state.unlockKey,
            ) { on ->
                if (on) {
                    actions.turnOnUnlock()
                } else {
                    stoppingUnlock = true
                }
            }
            if (state.unlockKey) {
                ToggleRow(
                    "Pop up the prompt",
                    "When you lock the PC by hand, or press a key at its lock screen. Off, tap the notification instead.",
                    settings.unlockPopUp,
                ) { set { copy(unlockPopUp = it) } }
                if (settings.unlockPopUp) {
                    SettingRow(
                        "Pop-ups",
                        if (setup.popUp) "Full-screen prompts and drawing over apps" else "Two Android settings: press again after the first",
                    ) {
                        Allowed(setup.popUp, onClick = actions.allowPopUp)
                    }
                    if (setup.hyperOs) {
                        SettingRow(
                            "Pop-ups on HyperOS",
                            "Allow \"Display pop-up windows while running in the background\" and \"Show on Lock screen\", or HyperOS only shows a banner",
                            onClick = actions.openHyperOsPermissions,
                        ) { LisButton("Open", actions.openHyperOsPermissions) }
                    }
                }
            }
        }

        SectionHeader("Find My Phone")
        ChoiceRow("Ring for", "Then it stops by itself", listOf(30, 60, 120, 300), settings.ringSeconds, ::seconds) {
            set { copy(ringSeconds = it) }
        }
        ToggleRow("Full volume", "Raises the alarm volume while it rings, then puts it back", settings.ringFullVolume) {
            set { copy(ringFullVolume = it) }
        }
        ToggleRow("Vibrate", null, settings.ringVibrate) { set { copy(ringVibrate = it) } }

        SectionHeader("Camera")
        ChoiceRow("Resolution", "Or the next one down, when the camera doesn't have it", CAMERA_SIZES.reversed(), settings.cameraSize, { it.label }) {
            set { copy(cameraHeight = it.height) }
        }
        ChoiceRow("Frame rate", null, listOf(30, 60), settings.cameraFps, { "$it fps" }) { set { copy(cameraFps = it) } }
        ChoiceRow("Picture in OBS", null, CameraAspect.entries, settings.cameraAspect, { it.label }) {
            set { copy(cameraAspect = it) }
        }
        ChoiceRow("Start with", null, listOf(false, true), settings.cameraFront, { if (it) "Front camera" else "Back camera" }) {
            set { copy(cameraFront = it) }
        }

        SectionHeader("Calendar")
        ChoiceRow("Weeks start on", null, listOf(false, true), settings.weekStartsMonday, { if (it) "Monday" else "Sunday" }) {
            set { copy(weekStartsMonday = it) }
        }

        SectionHeader("System")
        SettingRow("Run in the background", "So Android doesn't stop the connection to save battery") {
            Allowed(setup.background, onClick = actions.askBackground)
        }
        if (setup.hyperOs) {
            SettingRow("Autostart", "Turn it on for Lis App too, or HyperOS can stop it when you close it", onClick = actions.openAutostart) {
                LisButton("Open", actions.openAutostart)
            }
        }
        SettingRow("App info", "Android's page for Lis App", onClick = actions.openAppSettings) { Forward() }

        if (pc != null) {
            SectionHeader("Paired PC")
            Column(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                InfoRow("Name", pc.name)
                InfoRow("Address", if (state.usbMode) "USB cable" else if (state.relayed) "Relay server" else "${pc.host}:${pc.port}")
                InfoRow("Status", shortStatus(state))
                InfoRow("Fingerprint", shortFingerprint(pc.fingerprint))
            }
            SettingRow(
                "Unpair",
                "Its todos, notes, goals and calendar are removed from this phone; the PC keeps its own",
                onClick = { unpairing = true },
                labelColor = Theme.danger,
            ) { LisButton("Unpair", { unpairing = true }, danger = true) }
        }

        SectionHeader("About")
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            InfoRow("Version", version)
            if (phoneFingerprint.isNotEmpty()) InfoRow("This phone", shortFingerprint(phoneFingerprint))
        }
        Spacer(Modifier.height(24.dp))
    }

    if (unpairing) {
        ConfirmDialog(
            "Unpair from ${pc?.name ?: "the PC"}?",
            "You'd need to pair again to reconnect. The todos, notes, goals and calendar on this phone are removed; the PC keeps its own.",
            "Unpair",
            confirm = actions.unpair,
            dismiss = { unpairing = false },
        )
    }
    if (stoppingUnlock) {
        ConfirmDialog(
            "Turn off fingerprint unlock?",
            "The key is deleted from this phone. To turn it on again, you'd approve it on the PC again.",
            "Turn Off",
            confirm = actions.turnOffUnlock,
            dismiss = { stoppingUnlock = false },
        )
    }
}
