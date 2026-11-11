package com.lecer.lisapp

import android.util.Log
import java.time.LocalDate
import java.util.Calendar
import kotlin.random.Random
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject
import uniffi.lis_ffi.LisException

// The shell's documents. Each item keeps its original JSON, so fields this version doesn't know
// about survive the trip. Rust keeps the copy on the phone and does the merging.

const val DAILY_LIST = 1L
const val NO_LIST = -1L

private fun objects(array: JSONArray?): List<JSONObject> =
    (0 until (array?.length() ?: 0)).mapNotNull { array?.optJSONObject(it) }

data class Task(
    val id: Long,
    val text: String,
    val description: String = "",
    val done: Boolean = false,
    val important: Boolean = false,
    val list: Long = NO_LIST,
    val raw: String = "{}",
) {
    fun toJson(): JSONObject = JSONObject(raw)
        .put("id", id)
        .put("text", text)
        .put("description", description)
        .put("done", done)
        .put("important", important)
        .put("list", list)

    companion object {
        fun from(json: JSONObject) = Task(
            id = json.optLong("id"),
            text = json.optString("text"),
            description = json.optString("description"),
            done = json.optBoolean("done"),
            important = json.optBoolean("important"),
            list = json.optLong("list", NO_LIST),
            raw = json.toString(),
        )
    }
}

data class TaskList(val id: Long, val name: String)

data class TodoDoc(
    val tasks: List<Task> = emptyList(),
    val lists: List<TaskList> = listOf(TaskList(DAILY_LIST, "Daily")),
    val trash: List<String> = emptyList(),
    val dailyDate: String = "",
) {
    /** Starred first, as in the shell. */
    fun listed(list: Long): List<Task> {
        val shown = if (list == NO_LIST) tasks else tasks.filter { it.list == list }
        return shown.filter { it.important } + shown.filterNot { it.important }
    }

    private fun trashed(task: Task, now: Long): String =
        task.toJson().put("listName", lists.firstOrNull { it.id == task.list }?.name?.trim() ?: "").put("deletedAt", now).toString()

    /** To the trash, which the shell can restore from. */
    fun discard(matches: (Task) -> Boolean): TodoDoc {
        val now = System.currentTimeMillis()
        return copy(tasks = tasks.filterNot(matches), trash = trash + tasks.filter(matches).map { trashed(it, now) })
    }

    /** Daily tasks go back to undone each day, here too, as the phone can be away from the PC. */
    fun forToday(today: String): TodoDoc =
        if (dailyDate.isEmpty() || dailyDate == today) {
            this
        } else {
            copy(dailyDate = today, tasks = tasks.map { if (it.list == DAILY_LIST && it.done) it.copy(done = false) else it })
        }

    fun toText(): String = JSONObject()
        .put("tasks", JSONArray(tasks.map { it.toJson() }))
        .put("lists", JSONArray(lists.map { JSONObject().put("id", it.id).put("name", it.name) }))
        .put("trash", JSONArray(trash.map { JSONObject(it) }))
        .put("dailyDate", dailyDate)
        .toString()

    companion object {
        fun parse(text: String): TodoDoc {
            val json = JSONObject(text)
            return TodoDoc(
                tasks = objects(json.optJSONArray("tasks")).map { Task.from(it) }.distinctBy { it.id },
                lists = objects(json.optJSONArray("lists")).map { TaskList(it.optLong("id"), it.optString("name")) },
                trash = objects(json.optJSONArray("trash")).map { it.toString() },
                dailyDate = json.optString("dailyDate"),
            )
        }
    }
}

data class Subgoal(val id: Long, val title: String, val done: Boolean = false)

data class Goal(
    val id: Long,
    val title: String,
    val done: Boolean = false,
    val created: Long = 0,
    val subgoals: List<Subgoal> = emptyList(),
    val raw: String = "{}",
) {
    fun toJson(): JSONObject = JSONObject(raw)
        .put("id", id)
        .put("title", title)
        .put("done", done)
        .put("created", created)
        .put("subgoals", JSONArray(subgoals.map { JSONObject().put("id", it.id).put("title", it.title).put("done", it.done) }))

    companion object {
        fun from(json: JSONObject) = Goal(
            id = json.optLong("id"),
            title = json.optString("title"),
            done = json.optBoolean("done"),
            created = json.optLong("created"),
            subgoals = objects(json.optJSONArray("subgoals")).map { Subgoal(it.optLong("id"), it.optString("title"), it.optBoolean("done")) },
            raw = json.toString(),
        )
    }
}

data class Note(
    val id: Long,
    val title: String,
    val body: String,
    val created: Long,
    val updated: Long,
    /** Left out of the file until it has been set, as in the shell. */
    val isPrivate: Boolean? = null,
    val raw: String = "{}",
) {
    fun toJson(): JSONObject {
        val json = JSONObject(raw)
            .put("id", id)
            .put("title", title)
            .put("body", body)
            .put("created", created)
            .put("updated", updated)
        isPrivate?.let { json.put("private", it) }
        return json
    }

    companion object {
        fun from(json: JSONObject) = Note(
            id = json.optLong("id"),
            title = json.optString("title"),
            body = json.optString("body"),
            created = json.optLong("created"),
            updated = json.optLong("updated"),
            isPrivate = if (json.has("private")) json.optBoolean("private") else null,
            raw = json.toString(),
        )
    }
}

/** A note on a day of the shell's calendar, all day or at a time, once or repeating. */
data class CalendarNote(
    val id: Long,
    val date: String,
    val text: String,
    /** One of [REPEATS]. */
    val repeat: String = "none",
    /** "HH:MM", or empty for all day. */
    val time: String = "",
    val raw: String = "{}",
) {
    /** `yearly` stays for older copies of the app; `repeat` wins where both are. */
    fun toJson(): JSONObject = JSONObject(raw)
        .put("id", id)
        .put("date", date)
        .put("text", text)
        .put("yearly", repeat == "yearly")
        .apply {
            if (repeat == "none") remove("repeat") else put("repeat", repeat)
            if (time.isEmpty()) remove("time") else put("time", time)
        }

    /** As the shell matches them: a yearly Feb 29 shows on Feb 28 in other years, a monthly 31st on a short month's last day. */
    fun fallsOn(day: LocalDate): Boolean {
        val key = day.toString()
        if (date == key) return true
        return when (repeat) {
            "yearly" -> {
                val monthDay = date.drop(5)
                monthDay == key.drop(5) || (monthDay == "02-29" && key.drop(5) == "02-28" && !day.isLeapYear)
            }
            "weekly", "monthly" -> {
                val start = runCatching { LocalDate.parse(date) }.getOrNull()
                when {
                    start == null || day.isBefore(start) -> false
                    repeat == "weekly" -> start.dayOfWeek == day.dayOfWeek
                    else -> minOf(start.dayOfMonth, day.lengthOfMonth()) == day.dayOfMonth
                }
            }
            else -> false
        }
    }

    companion object {
        val REPEATS = listOf("none", "weekly", "monthly", "yearly")
        private val TIME = Regex("\\d{2}:\\d{2}")

        fun from(json: JSONObject) = CalendarNote(
            id = json.optLong("id"),
            date = json.optString("date"),
            text = json.optString("text"),
            repeat = json.optString("repeat").takeIf { it in REPEATS } ?: if (json.optBoolean("yearly")) "yearly" else "none",
            time = json.optString("time").takeIf { TIME.matches(it) } ?: "",
            raw = json.toString(),
        )
    }
}

private fun parseList(text: String): List<JSONObject> = objects(JSONArray(text))

/** One document on this phone. Edits show at once, and are saved after a pause in the typing. */
class SyncedDoc<T>(
    private val kind: String,
    empty: T,
    private val parse: (String) -> T,
    private val write: (T) -> String,
) {
    private val scope = CoroutineScope(Dispatchers.Default + SupervisorJob())
    private val mutable = MutableStateFlow(empty)
    private val externalChanges = MutableStateFlow(0)
    private val unsaved = mutableListOf<(T) -> T>()
    private var saving: Job? = null
    /** The copy these edits were made on, so Rust can tell them from what arrived since. */
    private var basedOn = ""

    val doc: StateFlow<T> = mutable

    /** Goes up when a change came from elsewhere, so an open editor knows to take it. */
    val external: StateFlow<Int> = externalChanges

    /** What's still being typed goes back on top of what arrived. */
    @Synchronized
    fun reload() {
        try {
            basedOn = Lis.client.syncDoc(kind)
            var fresh = parse(basedOn)
            unsaved.forEach { fresh = it(fresh) }
            mutable.value = fresh
            externalChanges.value += 1
        } catch (error: JSONException) {
            Log.w(TAG, "can't read $kind", error)
        }
    }

    @Synchronized
    fun edit(change: (T) -> T) {
        mutable.value = change(mutable.value)
        unsaved += change
        saving?.cancel()
        saving = scope.launch {
            delay(SAVE_AFTER_MS)
            flush()
        }
    }

    @Synchronized
    fun flush() {
        saving?.cancel()
        saving = null
        if (unsaved.isEmpty()) return
        unsaved.clear()
        try {
            val edited = Lis.client.syncEdit(kind, basedOn, write(mutable.value))
            basedOn = edited.doc
            if (edited.merged) {
                mutable.value = parse(edited.doc)
                externalChanges.value += 1
            }
        } catch (error: LisException) {
            Log.w(TAG, "can't save $kind", error)
            Lis.update { copy(syncError = error.message) }
        } catch (error: JSONException) {
            Log.w(TAG, "can't read $kind", error)
        }
    }

    private companion object {
        const val TAG = "LisSync"
        const val SAVE_AFTER_MS = 400L
    }
}

object SyncDocs {
    val todo = SyncedDoc(
        kind = "todo",
        empty = TodoDoc(),
        parse = { TodoDoc.parse(it) },
        write = TodoDoc::toText,
    )
    val goals = SyncedDoc(
        kind = "goals",
        empty = emptyList<Goal>(),
        parse = { text -> parseList(text).map { Goal.from(it) }.distinctBy { it.id } },
        write = { list -> JSONArray(list.map { it.toJson() }).toString() },
    )
    val notes = SyncedDoc(
        kind = "notes",
        empty = emptyList<Note>(),
        parse = { text -> parseList(text).map { Note.from(it) }.distinctBy { it.id } },
        write = { list -> JSONArray(list.map { it.toJson() }).toString() },
    )
    val calendar = SyncedDoc(
        kind = "calendar",
        empty = emptyList<CalendarNote>(),
        parse = { text -> parseList(text).map { CalendarNote.from(it) }.distinctBy { it.id } },
        write = { list -> JSONArray(list.map { it.toJson() }).toString() },
    )

    private var lastId = 0L

    /** Like the shell's, with some randomness as the two can pick one in the same millisecond. */
    @Synchronized
    fun newId(): Long {
        lastId = maxOf(System.currentTimeMillis(), lastId + 1) + Random.nextInt(1000)
        return lastId
    }

    fun reload(kind: String) {
        when (kind) {
            "todo" -> todo.reload()
            "goals" -> goals.reload()
            "notes" -> notes.reload()
            "calendar" -> calendar.reload()
        }
    }

    fun reloadAll() {
        todo.reload()
        goals.reload()
        notes.reload()
        calendar.reload()
    }

    fun flushAll() {
        todo.flush()
        goals.flush()
        notes.flush()
        calendar.flush()
    }

    fun clear() = reloadAll()

    private fun today(): String {
        val now = Calendar.getInstance()
        return "${now.get(Calendar.YEAR)}-${now.get(Calendar.MONTH) + 1}-${now.get(Calendar.DAY_OF_MONTH)}"
    }

    fun checkDay() {
        val day = today()
        if (todo.doc.value.forToday(day) != todo.doc.value) todo.edit { it.forToday(day) }
    }
}
