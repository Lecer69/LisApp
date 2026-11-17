package com.lecer.lisapp

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.LocalDate
import kotlin.math.roundToInt
import uniffi.lis_ffi.ConnectionState
import uniffi.lis_ffi.MediaCommand
import uniffi.lis_ffi.MediaInfo
import uniffi.lis_ffi.PcStats

private const val DEFAULT_PORT = 47420

sealed interface Screen {
    data object Home : Screen
    data object Settings : Screen
    data object Camera : Screen
    data object Logs : Screen
    data class Organizer(val page: OrganizerPage) : Screen
}

data class SetupState(
    val notifications: Boolean = true,
    val background: Boolean = true,
    val popUp: Boolean = true,
    val hyperOs: Boolean = false,
) {
    /** What the app can't do its job without, marked on the settings button until it's allowed. */
    val missing: Boolean get() = !notifications || !background
}

class ScreenActions(
    val pair: (String, Int) -> Unit,
    val answerCode: (Boolean) -> Unit,
    val lockPc: () -> Unit,
    val sendFile: () -> Unit,
    val openCamera: () -> Unit,
    val openPage: (OrganizerPage) -> Unit,
    val openSettings: () -> Unit,
    val openLogs: () -> Unit,
    val stopRinging: () -> Unit,
    val mediaCommand: (MediaCommand) -> Unit,
    val setVolume: (Int) -> Unit,
    val runAction: (String) -> Unit,
    val unlockPc: () -> Unit,
)

@Composable
fun LisScreen(state: AppState, settings: AppSettings, setup: SetupState, actions: ScreenActions) {
    if (state.paired == null) PairScreen(state, setup, actions) else HomeScreen(state, settings, setup, actions)
    state.pairCode?.let { code -> CodeDialog(code, actions.answerCode) }
}

fun shortStatus(state: AppState): String = when (state.connection) {
    ConnectionState.CONNECTED -> state.pcStats?.let { "Connected · up ${duration(it.uptimeSecs)}" } ?: "Connected"
    ConnectionState.CONNECTING -> "Connecting…"
    ConnectionState.UNPAIRED, ConnectionState.WRONG_PC -> "Not connected"
    ConnectionState.DISCONNECTED -> when {
        state.usbMode -> "Waiting for the USB cable…"
        state.paused -> "Paused while Wi-Fi is off"
        else -> "Looking for the PC…"
    }
}

private fun problem(state: AppState): String? = when (state.connection) {
    ConnectionState.UNPAIRED -> "The PC no longer recognises this phone. Unpair in Settings, then pair again."
    ConnectionState.WRONG_PC -> "Another device answered at this address, so nothing was sent to it."
    else -> null
}

@Composable
private fun SettingsButton(setup: SetupState, open: () -> Unit) {
    IconTap(R.drawable.ic_settings, "Settings", open, dot = setup.missing)
}

// Pairing

@Composable
private fun PairScreen(state: AppState, setup: SetupState, actions: ScreenActions) {
    var address by remember { mutableStateOf("") }
    val connect: () -> Unit = {
        val host = address.substringBefore(':')
        val port = address.substringAfter(':', "").toIntOrNull()?.takeIf { it in 1..65535 } ?: DEFAULT_PORT
        if (host.isNotEmpty()) actions.pair(host, port)
    }

    ScreenColumn {
        ScreenHeader("Lis App", "Pair with your PC") { SettingsButton(setup, actions.openSettings) }

        Panel(title = "How to pair") {
            Step(1, "On the PC, open Settings, then Phone Sync, and press Pair.")
            Step(2, "Choose the PC below, or enter its address.")
            Step(3, "Accept on both if they show the same code.")
        }

        SectionHeader("PCs on this network", start = 4.dp)
        if (state.found.isEmpty()) {
            StatusLine("Looking…")
        }
        state.found.forEach { pc -> FoundRow(pc) { actions.pair(pc.host, pc.port) } }

        SectionHeader("Or enter its address", start = 4.dp)
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Field(
                value = address,
                onChange = { address = it.trim() },
                placeholder = "192.168.1.10",
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Go),
                keyboardActions = KeyboardActions(onGo = { connect() }),
                modifier = Modifier.weight(1f),
            )
            LisButton("Connect", connect, enabled = address.isNotEmpty())
        }

        when (state.connection) {
            ConnectionState.CONNECTING -> StatusLine("Connecting…")
            ConnectionState.UNPAIRED -> StatusLine("Connected to ${state.pcName ?: "the PC"}, not paired yet")
            else -> {}
        }
        state.pairMessage?.let { StatusLine(it, error = true) }
    }
}

@Composable
private fun Step(number: Int, text: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.Top) {
        Box(
            modifier = Modifier.size(22.dp).clip(CircleShape).background(Theme.border),
            contentAlignment = Alignment.Center,
        ) {
            Text("$number", fontSize = 12.sp, fontWeight = FontWeight.Medium, color = Theme.text)
        }
        Text(text, fontSize = 14.sp, color = Theme.text, modifier = Modifier.padding(top = 1.dp))
    }
}

@Composable
private fun FoundRow(pc: FoundPc, onClick: () -> Unit) {
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val shape = RoundedCornerShape(12.dp)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(if (pressed) Theme.hover else Theme.panel)
            .border(1.dp, Theme.border, shape)
            .clickable(interactionSource = source, indication = null, role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Glyph(R.drawable.ic_monitor, size = 22.dp)
        Column(modifier = Modifier.weight(1f)) {
            Text(pc.name, fontSize = 15.sp, color = Theme.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Dim(pc.host)
        }
        Glyph(R.drawable.ic_forward, size = 18.dp, tint = Theme.faint)
    }
}

@Composable
private fun CodeDialog(code: String, answer: (Boolean) -> Unit) {
    PanelDialog(
        title = "Does the PC show this code?",
        dismiss = {},
        buttons = {
            LisButton("No", { answer(false) })
            LisButton("Yes, Pair", { answer(true) })
        },
    ) {
        Text(
            code.chunked(3).joinToString(" "),
            fontSize = 38.sp,
            fontWeight = FontWeight.SemiBold,
            color = Theme.strong,
            textAlign = TextAlign.Center,
            letterSpacing = 2.sp,
            modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
        )
        Dim("Pair only if it's the same, as anything else may be listening in.")
    }
}

// Home

@Composable
private fun HomeScreen(state: AppState, settings: AppSettings, setup: SetupState, actions: ScreenActions) {
    val name = state.paired?.name ?: "PC"
    val connected = state.connection == ConnectionState.CONNECTED
    var confirmingLock by remember { mutableStateOf(false) }
    val lock: () -> Unit = {
        if (settings.confirmLock) {
            confirmingLock = true
        } else {
            actions.lockPc()
        }
    }

    ScreenColumn {
        ScreenHeader(name, shortStatus(state)) {
            IconTap(R.drawable.ic_logs, "Logs", actions.openLogs)
            SettingsButton(setup, actions.openSettings)
        }

        problem(state)?.let { StatusLine(it, error = true) }

        if (state.ringing) {
            Banner(R.drawable.ic_bell, "Your PC is looking for this phone", "Stop", actions.stopRinging)
        }
        if (connected && state.pcLocked && state.unlockAvailable) {
            Banner(R.drawable.ic_lock, "$name is locked", "Unlock", actions.unlockPc)
        }
        state.unlockMessage?.let { StatusLine(it) }

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
            Tile(R.drawable.ic_lock, "Lock PC", lock, modifier = Modifier.weight(1f), enabled = connected)
            Tile(R.drawable.ic_upload, "Send File", actions.sendFile, modifier = Modifier.weight(1f), enabled = connected)
            Tile(R.drawable.ic_camera, "Camera", actions.openCamera, modifier = Modifier.weight(1f), enabled = connected)
        }
        state.transfer?.let { StatusLine(it) }

        if (settings.showPcStats && connected) state.pcStats?.let { PcPanel(it) }
        if (settings.showMedia && connected) state.media?.let { MediaPanel(it, actions) }
        if (settings.showOrganizer) OrganizerPanel(actions.openPage)
        state.syncError?.let { StatusLine(it, error = true) }
        if (settings.showActions && connected && state.actions.isNotEmpty()) ActionsPanel(state, actions)

        Spacer(Modifier.height(8.dp))
    }

    if (confirmingLock) {
        PanelDialog(
            title = "Lock $name?",
            dismiss = { confirmingLock = false },
            buttons = {
                LisButton("Cancel", { confirmingLock = false })
                LisButton("Lock", {
                    confirmingLock = false
                    actions.lockPc()
                })
            },
        )
    }
}

@Composable
private fun Banner(icon: Int, text: String, action: String, onClick: () -> Unit) {
    val shape = RoundedCornerShape(12.dp)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Theme.hover)
            .border(1.dp, Theme.track, shape)
            .padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Glyph(icon, size = 20.dp)
        Text(text, fontSize = 15.sp, color = Theme.text, modifier = Modifier.weight(1f))
        LisButton(action, onClick)
    }
}

@Composable
private fun PcPanel(stats: PcStats) {
    Panel(title = "System", subtitle = stats.hostname) {
        Meter("CPU", stats.cpuPercent / 100f, "${stats.cpuPercent.roundToInt()}%")
        Meter(
            "Memory",
            if (stats.memoryTotal > 0u) (stats.memoryUsed.toDouble() / stats.memoryTotal.toDouble()).toFloat() else 0f,
            "${gigabytes(stats.memoryUsed)} / ${gigabytes(stats.memoryTotal)}",
        )
        InfoRow("Up", duration(stats.uptimeSecs))
    }
}

@Composable
private fun MediaPanel(media: MediaInfo, actions: ScreenActions) {
    Panel(title = "Now Playing", subtitle = media.player) {
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                media.title.ifEmpty { "Unknown" },
                fontSize = 16.sp,
                fontWeight = FontWeight.Medium,
                color = Theme.text,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            val credit = listOf(media.artist, media.album).filter { it.isNotEmpty() }.joinToString(" · ")
            if (credit.isNotEmpty()) Dim(credit, maxLines = 1, size = 14.sp)
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(20.dp, Alignment.CenterHorizontally),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconTap(R.drawable.ic_previous, "Previous", { actions.mediaCommand(MediaCommand.PREVIOUS) }, size = 48.dp, iconSize = 22.dp, tint = Theme.fill)
            PlayButton(media.playing) { actions.mediaCommand(MediaCommand.PLAY_PAUSE) }
            IconTap(R.drawable.ic_next, "Next", { actions.mediaCommand(MediaCommand.NEXT) }, size = 48.dp, iconSize = 22.dp, tint = Theme.fill)
        }
        VolumeSlider(media, actions.setVolume)
    }
}

/** The shell's accent transport button: the text color, with the icon in the panel's. */
@Composable
private fun PlayButton(playing: Boolean, onClick: () -> Unit) {
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    Box(
        modifier = Modifier
            .size(56.dp)
            .clip(CircleShape)
            .background(if (pressed) Theme.fill else Theme.text)
            .clickable(interactionSource = source, indication = null, role = Role.Button, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Glyph(
            if (playing) R.drawable.ic_pause else R.drawable.ic_play,
            size = 24.dp,
            tint = Theme.panel,
            description = if (playing) "Pause" else "Play",
        )
    }
}

/** Only sent once let go, and follows the PC otherwise. */
@Composable
private fun VolumeSlider(media: MediaInfo, setVolume: (Int) -> Unit) {
    var dragged by remember { mutableFloatStateOf(-1f) }
    val percent = media.volumePercent.toInt()
    LaunchedEffect(percent) { dragged = -1f }
    val shown = if (dragged >= 0f) dragged else percent / 100f
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        InfoRow("Volume", if (media.muted) "Muted" else "${(shown * 100).roundToInt()}%")
        LisSlider(
            value = shown,
            onChange = { dragged = it },
            done = { if (dragged >= 0f) setVolume((dragged * 100).roundToInt()) },
        )
    }
}

@Composable
private fun OrganizerPanel(open: (OrganizerPage) -> Unit) {
    val todo by SyncDocs.todo.doc.collectAsState()
    val notes by SyncDocs.notes.doc.collectAsState()
    val goals by SyncDocs.goals.doc.collectAsState()
    val calendar by SyncDocs.calendar.doc.collectAsState()

    val openTasks = todo.tasks.count { !it.done }
    val today = LocalDate.now()
    val todayNotes = calendar.count { it.fallsOn(today) }
    val detail = mapOf(
        OrganizerPage.TODOS to (if (openTasks == 0) "All done" else "$openTasks open"),
        OrganizerPage.NOTES to "${notes.size} ${if (notes.size == 1) "note" else "notes"}",
        OrganizerPage.GOALS to (if (goals.isEmpty()) "None yet" else "${goals.count { it.done }} of ${goals.size} done"),
        OrganizerPage.CALENDAR to (if (todayNotes == 0) "Nothing today" else "$todayNotes today"),
    )

    Panel(title = "Organizer", subtitle = "Synced with the shell") {
        OrganizerPage.entries.chunked(2).forEach { pair ->
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                pair.forEach { page ->
                    Tile(page.icon, page.title, { open(page) }, modifier = Modifier.weight(1f), detail = detail[page])
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ActionsPanel(state: AppState, actions: ScreenActions) {
    Panel(title = "Actions") {
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            state.actions.forEach { action ->
                LisButton(action.name, { actions.runAction(action.id) }, modifier = Modifier.heightIn(min = 42.dp))
            }
        }
        state.actionMessage?.let { Dim(it) }
    }
}

fun gigabytes(bytes: ULong): String = "%.1f GB".format(bytes.toDouble() / (1024.0 * 1024 * 1024))

fun duration(seconds: ULong): String {
    val minutes = (seconds / 60u).toLong()
    val hours = minutes / 60
    val days = hours / 24
    return when {
        days > 0 -> "${days}d ${hours % 24}h"
        hours > 0 -> "${hours}h ${minutes % 60}m"
        else -> "${minutes}m"
    }
}
