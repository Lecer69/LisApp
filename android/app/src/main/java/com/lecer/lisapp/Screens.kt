package com.lecer.lisapp

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import uniffi.lis_ffi.ConnectionState
import uniffi.lis_ffi.PcStats

private const val DEFAULT_PORT = 47420

data class SetupState(
    val notifications: Boolean = true,
    val background: Boolean = true,
)

class ScreenActions(
    val pair: (String, Int) -> Unit,
    val answerCode: (Boolean) -> Unit,
    val lockPc: () -> Unit,
    val sendFile: () -> Unit,
    val openCamera: () -> Unit,
    val unpair: () -> Unit,
    val askNotifications: () -> Unit,
    val askBackground: () -> Unit,
    val openAutostart: () -> Unit,
)

@Composable
fun LisScreen(state: AppState, setup: SetupState, actions: ScreenActions) {
    Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .safeDrawingPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            if (state.paired == null) PairScreen(state, actions) else HomeScreen(state, setup, actions)
        }

        state.pairCode?.let { code -> CodeDialog(code, actions.answerCode) }
    }
}

@Composable
private fun Title(text: String, subtitle: String? = null) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(text, fontSize = 24.sp, fontWeight = FontWeight.SemiBold)
        subtitle?.let { Secondary(it) }
    }
}

@Composable
private fun Secondary(text: String) {
    Text(text, fontSize = 14.sp, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun SectionLabel(text: String) {
    Text(text, fontSize = 13.sp, fontWeight = FontWeight.Medium, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun Card(onClick: (() -> Unit)? = null, content: @Composable () -> Unit) {
    val shape = RoundedCornerShape(12.dp)
    Surface(
        shape = shape,
        color = MaterialTheme.colorScheme.surfaceVariant,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        modifier = Modifier.fillMaxWidth().let { if (onClick != null) it.clickable(onClick = onClick) else it },
    ) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            content()
        }
    }
}

@Composable
private fun PairScreen(state: AppState, actions: ScreenActions) {
    var address by remember { mutableStateOf("") }

    Title("Lis App", "Pair with your PC")
    Secondary("On the PC, open Settings, then Phone Sync, and press Pair. Then choose it here.")

    SectionLabel("PCs on this network")
    if (state.found.isEmpty()) {
        Secondary("Looking…")
    }
    state.found.forEach { pc ->
        Card(onClick = { actions.pair(pc.host, pc.port) }) {
            Text(pc.name, fontSize = 16.sp)
            Secondary(pc.host)
        }
    }

    SectionLabel("Or enter its address")
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedTextField(
            value = address,
            onValueChange = { address = it.trim() },
            singleLine = true,
            placeholder = { Text("192.168.1.10") },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            modifier = Modifier.weight(1f),
        )
        OutlinedButton(
            enabled = address.isNotEmpty(),
            onClick = {
                val host = address.substringBefore(':')
                val port = address.substringAfter(':', "").toIntOrNull()?.takeIf { it in 1..65535 } ?: DEFAULT_PORT
                actions.pair(host, port)
            },
        ) {
            Text("Connect")
        }
    }

    when (state.connection) {
        ConnectionState.CONNECTING -> Secondary("Connecting…")
        ConnectionState.UNPAIRED -> Secondary("Connected to ${state.pcName ?: "the PC"}, not paired yet")
        else -> {}
    }
    state.pairMessage?.let { Text(it, fontSize = 14.sp, color = MaterialTheme.colorScheme.error) }
}

@Composable
private fun CodeDialog(code: String, answer: (Boolean) -> Unit) {
    AlertDialog(
        onDismissRequest = {},
        title = { Text("Does the PC show this code?") },
        text = {
            Text(
                code.chunked(3).joinToString(" "),
                fontSize = 36.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = {
            TextButton(onClick = { answer(true) }) { Text("Yes, pair") }
        },
        dismissButton = {
            TextButton(onClick = { answer(false) }) { Text("No") }
        },
    )
}

@Composable
private fun HomeScreen(state: AppState, setup: SetupState, actions: ScreenActions) {
    val name = state.paired?.name ?: "PC"
    val status = when (state.connection) {
        ConnectionState.CONNECTED -> "Connected"
        ConnectionState.CONNECTING -> "Connecting…"
        ConnectionState.UNPAIRED -> "The PC no longer recognises this phone. Unpair and pair again."
        ConnectionState.WRONG_PC -> "Another device answered at this address, so nothing was sent to it"
        ConnectionState.DISCONNECTED -> "Looking for the PC…"
    }
    val connected = state.connection == ConnectionState.CONNECTED

    Title(name, status)

    state.pcStats?.takeIf { connected }?.let { PcFigures(it) }

    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
        Button(
            enabled = connected,
            onClick = actions.lockPc,
            modifier = Modifier.weight(1f),
            colors = ButtonDefaults.buttonColors(),
        ) {
            Text("Lock PC")
        }
        OutlinedButton(enabled = connected, onClick = actions.sendFile, modifier = Modifier.weight(1f)) {
            Text("Send file")
        }
    }
    OutlinedButton(enabled = connected, onClick = actions.openCamera, modifier = Modifier.fillMaxWidth()) {
        Text("Camera")
    }
    state.transfer?.let { Secondary(it) }

    SetupList(setup, actions)

    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    UnpairButton(name, actions.unpair)
}

@Composable
private fun PcFigures(stats: PcStats) {
    Card {
        Figure("CPU", "${stats.cpuPercent.toInt()}%")
        Figure("Memory", "${gigabytes(stats.memoryUsed)} / ${gigabytes(stats.memoryTotal)}")
        Figure("Up", duration(stats.uptimeSecs))
    }
}

@Composable
private fun Figure(label: String, value: String) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
        Secondary(label)
        Text(value, fontSize = 14.sp)
    }
}

private fun gigabytes(bytes: ULong): String = "%.1f GB".format(bytes.toDouble() / (1024.0 * 1024 * 1024))

private fun duration(seconds: ULong): String {
    val minutes = (seconds / 60u).toLong()
    val hours = minutes / 60
    val days = hours / 24
    return when {
        days > 0 -> "${days}d ${hours % 24}h"
        hours > 0 -> "${hours}h ${minutes % 60}m"
        else -> "${minutes}m"
    }
}

@Composable
private fun SetupList(setup: SetupState, actions: ScreenActions) {
    SectionLabel("Setup")
    if (!setup.notifications) {
        SetupItem("Allow notifications", "To show the connection and finished transfers.", "Allow", actions.askNotifications)
    }
    if (!setup.background) {
        SetupItem("Run in the background", "So the system doesn't stop the connection to save battery.", "Allow", actions.askBackground)
    }
    SetupItem(
        title = "Autostart",
        detail = "On HyperOS, turn on Autostart for Lis App too, or it can be stopped when you close it.",
        action = "Open",
        onClick = actions.openAutostart,
    )
}

@Composable
private fun SetupItem(
    title: String,
    detail: String,
    action: String,
    onClick: () -> Unit,
) {
    Card {
        Text(title, fontSize = 15.sp)
        Secondary(detail)
        Spacer(Modifier.height(4.dp))
        OutlinedButton(onClick = onClick) { Text(action) }
    }
}

@Composable
private fun UnpairButton(name: String, unpair: () -> Unit) {
    var asking by remember { mutableStateOf(false) }
    TextButton(onClick = { asking = true }) {
        Text("Unpair", color = MaterialTheme.colorScheme.error)
    }
    if (asking) {
        AlertDialog(
            onDismissRequest = { asking = false },
            title = { Text("Unpair from $name?") },
            text = { Text("You'd need to pair again to reconnect.") },
            confirmButton = {
                TextButton(onClick = {
                    asking = false
                    unpair()
                }) { Text("Unpair", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = {
                TextButton(onClick = { asking = false }) { Text("Cancel") }
            },
        )
    }
}
