package com.lecer.lisapp

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter

private val TIME = DateTimeFormatter.ofPattern("HH:mm")
private val DAY = DateTimeFormatter.ofPattern("EEEE, d MMMM")

/** The PC turning on, connections, and files both ways, newest first, by day. */
@Composable
fun LogsScreen(back: () -> Unit) {
    val events by EventLog.events.collectAsState()
    val zone = ZoneId.systemDefault()
    val today = LocalDate.now(zone)

    ScreenColumn {
        ScreenHeader("Logs", if (events.isEmpty()) null else "${events.size} events", back) {
            if (events.isNotEmpty()) LisButton("Clear", EventLog::clear)
        }

        if (events.isEmpty()) {
            StatusLine("Nothing yet. Connections, the PC turning on and files show here.")
        }

        events.groupBy { Instant.ofEpochMilli(it.at).atZone(zone).toLocalDate() }.forEach { (day, list) ->
            val title = when (day) {
                today -> "Today"
                today.minusDays(1) -> "Yesterday"
                else -> day.format(DAY)
            }
            Panel(title = title) {
                list.forEach { event -> LogRow(event, zone) }
            }
        }
    }
}

@Composable
private fun LogRow(event: LogEvent, zone: ZoneId) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Text(
            Instant.ofEpochMilli(event.at).atZone(zone).format(TIME),
            fontSize = 13.sp,
            color = Theme.textDim,
        )
        Column(modifier = Modifier.weight(1f)) {
            Text(
                event.text,
                fontSize = 14.sp,
                color = if (event.failed) Theme.danger else Theme.text,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            val detail = listOfNotNull(kindLabel(event.kind), event.size?.let(::bytes)).joinToString(" · ")
            Text(detail, fontSize = 12.sp, color = Theme.textDim, modifier = Modifier.padding(top = 2.dp))
        }
    }
}

private fun kindLabel(kind: EventKind): String = when (kind) {
    EventKind.PC_ON -> "Turned on"
    EventKind.CONNECTED -> "Connection"
    EventKind.DISCONNECTED -> "Connection"
    EventKind.FILE_SENT -> "To the PC"
    EventKind.FILE_RECEIVED -> "From the PC"
}

private fun bytes(size: Long): String = when {
    size < 1024 -> "$size B"
    size < 1024 * 1024 -> "%.1f KB".format(size / 1024.0)
    size < 1024L * 1024 * 1024 -> "%.1f MB".format(size / (1024.0 * 1024))
    else -> "%.2f GB".format(size / (1024.0 * 1024 * 1024))
}
