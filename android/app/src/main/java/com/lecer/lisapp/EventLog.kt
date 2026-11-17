package com.lecer.lisapp

import android.content.Context
import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

enum class EventKind { PC_ON, CONNECTED, DISCONNECTED, FILE_SENT, FILE_RECEIVED }

data class LogEvent(
    val kind: EventKind,
    val at: Long,
    val text: String,
    /** Bytes, for file events. */
    val size: Long? = null,
    val failed: Boolean = false,
)

/** What happened with the PC, kept in one small file so it survives restarts. */
object EventLog {
    private const val TAG = "LisLog"
    private const val FILE = "events.json"
    private const val KEEP = 200

    private lateinit var file: File
    private val mutable = MutableStateFlow<List<LogEvent>>(emptyList())
    val events: StateFlow<List<LogEvent>> = mutable

    fun init(context: Context) {
        file = File(context.filesDir, FILE)
        mutable.value = read()
    }

    fun add(kind: EventKind, text: String, size: Long? = null, failed: Boolean = false) {
        if (!::file.isInitialized) return
        val event = LogEvent(kind, System.currentTimeMillis(), text, size, failed)
        mutable.update { (listOf(event) + it).take(KEEP) }
        write()
    }

    fun clear() {
        if (!::file.isInitialized) return
        mutable.value = emptyList()
        write()
    }

    private fun read(): List<LogEvent> = try {
        if (!file.exists()) {
            emptyList()
        } else {
            val array = JSONArray(file.readText())
            (0 until array.length()).mapNotNull { index ->
                val row = array.optJSONObject(index) ?: return@mapNotNull null
                val kind = runCatching { EventKind.valueOf(row.optString("kind")) }.getOrNull() ?: return@mapNotNull null
                LogEvent(
                    kind = kind,
                    at = row.optLong("at"),
                    text = row.optString("text"),
                    size = if (row.has("size")) row.optLong("size") else null,
                    failed = row.optBoolean("failed"),
                )
            }
        }
    } catch (error: Exception) {
        Log.w(TAG, "can't read the log", error)
        emptyList()
    }

    private fun write() {
        val array = JSONArray()
        for (event in mutable.value) {
            val row = JSONObject()
                .put("kind", event.kind.name)
                .put("at", event.at)
                .put("text", event.text)
                .put("failed", event.failed)
            event.size?.let { row.put("size", it) }
            array.put(row)
        }
        try {
            file.writeText(array.toString())
        } catch (error: Exception) {
            Log.w(TAG, "can't save the log", error)
        }
    }
}
