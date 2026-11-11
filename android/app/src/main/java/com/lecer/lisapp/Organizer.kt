package com.lecer.lisapp

import android.app.TimePickerDialog
import android.content.Context
import android.content.DialogInterface
import androidx.activity.compose.BackHandler
import androidx.annotation.DrawableRes
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.DayOfWeek
import java.time.Instant
import java.time.LocalDate
import java.time.LocalTime
import java.time.YearMonth
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

/** The shell's todos, notes, goals and calendar. They work without the PC, and catch up when it's back. */
enum class OrganizerPage(val title: String, @DrawableRes val icon: Int) {
    TODOS("Todos", R.drawable.ic_todo),
    NOTES("Notes", R.drawable.ic_notes),
    GOALS("Goals", R.drawable.ic_goals),
    CALENDAR("Calendar", R.drawable.ic_calendar),
}

@Composable
fun OrganizerScreen(page: OrganizerPage, close: () -> Unit) {
    when (page) {
        OrganizerPage.TODOS -> TodosPage(close)
        OrganizerPage.NOTES -> NotesPage(close)
        OrganizerPage.GOALS -> GoalsPage(close)
        OrganizerPage.CALENDAR -> CalendarPage(close)
    }
}

private val RowShape = RoundedCornerShape(8.dp)

/** A word to press, like the shell's small text buttons. */
@Composable
private fun SmallAction(text: String, danger: Boolean = false, onClick: () -> Unit) {
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    Box(
        modifier = Modifier
            .clip(RowShape)
            .background(if (pressed) Theme.hover else Color.Transparent)
            .clickable(interactionSource = source, indication = null, onClick = onClick)
            .heightIn(min = 36.dp)
            .padding(horizontal = 12.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, fontSize = 14.sp, color = if (danger) Theme.danger else Theme.faint)
    }
}

/** A row of a list: lit while pressed or open. Only its line opens it, not the details under it. */
@Composable
private fun ItemRow(
    open: Boolean = false,
    onClick: () -> Unit,
    details: @Composable () -> Unit = {},
    content: @Composable () -> Unit,
) {
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RowShape)
            .background(if (open || pressed) Theme.hover else Color.Transparent),
    ) {
        Box(modifier = Modifier.fillMaxWidth().clickable(interactionSource = source, indication = null, onClick = onClick)) {
            content()
        }
        details()
    }
}

@Composable
private fun Empty(text: String) {
    Text(
        text,
        fontSize = 14.sp,
        color = Theme.textDim,
        textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxWidth().padding(vertical = 32.dp),
    )
}

/**
 * Keeps what's typed here, as the document it edits can lag a frame behind, and a field following it
 * would lose letters. It only takes the document's value when a change came from elsewhere.
 */
@Composable
private fun EditField(
    external: Int,
    current: () -> String?,
    onChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    placeholder: String = "",
    singleLine: Boolean = false,
) {
    var text by remember { mutableStateOf(current().orEmpty()) }
    LaunchedEffect(external) { current()?.let { text = it } }
    Field(
        value = text,
        onChange = {
            text = it
            onChange(it)
        },
        singleLine = singleLine,
        placeholder = placeholder,
        fontSize = 14.sp,
        modifier = modifier,
    )
}

// Todos

@Composable
private fun TodosPage(close: () -> Unit) {
    val doc by SyncDocs.todo.doc.collectAsState()
    var list by remember { mutableLongStateOf(NO_LIST) }
    var opened by remember { mutableStateOf<Long?>(null) }
    var naming by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf(false) }

    // The list can be gone after a sync
    val current = if (doc.lists.any { it.id == list }) list else NO_LIST
    val shown = doc.listed(current)
    val open = shown.count { !it.done }

    ScreenColumn(scroll = false, spacing = 8.dp) {
        ScreenHeader("Todos", if (shown.isEmpty()) null else "$open open", back = close)

        Row(verticalAlignment = Alignment.CenterVertically) {
            Chips(
                options = listOf(NO_LIST) + doc.lists.map { it.id },
                selected = current,
                name = { id -> if (id == NO_LIST) "All" else doc.lists.firstOrNull { it.id == id }?.name ?: "" },
                modifier = Modifier.weight(1f),
            ) { list = it }
            IconTap(R.drawable.ic_plus, "New list", { naming = true }, size = 40.dp, iconSize = 18.dp, tint = Theme.faint)
        }

        if (current != NO_LIST && current != DAILY_LIST) {
            Row(modifier = Modifier.offset(x = (-12).dp)) {
                SmallAction("Rename list") { renaming = true }
                SmallAction("Delete list", danger = true) { removing = true }
            }
        }

        LazyColumn(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            if (shown.isEmpty()) {
                item { Empty(if (current == NO_LIST) "No tasks yet" else "Nothing in this list") }
            }
            items(shown, key = { it.id }) { task ->
                TaskRow(
                    task = task,
                    lists = doc.lists,
                    showList = current == NO_LIST,
                    open = opened == task.id,
                ) { opened = if (opened == task.id) null else task.id }
            }
            if (shown.any { it.done && it.list != DAILY_LIST }) {
                item {
                    Box(modifier = Modifier.offset(x = (-4).dp)) {
                        SmallAction("Clear done") {
                            val cleared = current
                            SyncDocs.todo.edit { todo ->
                                todo.discard { it.done && it.list != DAILY_LIST && (cleared == NO_LIST || it.list == cleared) }
                            }
                        }
                    }
                }
            }
        }

        AddField("New task", add = { text ->
            val id = SyncDocs.newId()
            val into = current
            SyncDocs.todo.edit { todo -> todo.copy(tasks = todo.tasks + Task(id, text, list = into)) }
        }, modifier = Modifier.padding(bottom = 8.dp))
    }

    if (naming) {
        NameDialog("New list", "", save = { name ->
            val id = SyncDocs.newId()
            SyncDocs.todo.edit { todo -> todo.copy(lists = todo.lists + TaskList(id, name)) }
            list = id
        }, dismiss = { naming = false })
    }
    if (renaming) {
        val target = current
        NameDialog("Rename list", doc.lists.firstOrNull { it.id == target }?.name ?: "", save = { name ->
            SyncDocs.todo.edit { todo -> todo.copy(lists = todo.lists.map { if (it.id == target) it.copy(name = name) else it }) }
        }, dismiss = { renaming = false })
    }
    if (removing) {
        val target = current
        ConfirmDialog("Delete this list?", "Its tasks go to the trash, which you can restore from on the PC.", "Delete", confirm = {
            SyncDocs.todo.edit { todo ->
                val rest = todo.discard { it.list == target }
                rest.copy(lists = rest.lists.filterNot { it.id == target })
            }
            list = NO_LIST
        }, dismiss = { removing = false })
    }
}

@Composable
private fun TaskRow(task: Task, lists: List<TaskList>, showList: Boolean, open: Boolean, toggleOpen: () -> Unit) {
    fun update(change: (Task) -> Task) {
        SyncDocs.todo.edit { todo -> todo.copy(tasks = todo.tasks.map { if (it.id == task.id) change(it) else it }) }
    }
    val listName = lists.firstOrNull { it.id == task.list }?.name

    ItemRow(open = open, onClick = toggleOpen, details = { if (open) TaskDetails(task, lists, ::update) }) {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.heightIn(min = 52.dp)) {
            Check(task.done, { done -> update { it.copy(done = done) } })
            Column(modifier = Modifier.weight(1f).padding(vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    task.text,
                    fontSize = 15.sp,
                    textDecoration = if (task.done) TextDecoration.LineThrough else null,
                    color = if (task.done) Theme.textDim else Theme.text,
                )
                if (!open && task.description.isNotEmpty()) {
                    Dim(task.description.trim(), maxLines = 2, color = if (task.done) Theme.textDim else Theme.value)
                }
                if (showList && listName != null) Dim(listName, maxLines = 1, size = 12.sp)
            }
            IconTap(
                if (task.important) R.drawable.ic_star_filled else R.drawable.ic_star,
                if (task.important) "Unstar" else "Star",
                { update { it.copy(important = !it.important) } },
                iconSize = 18.dp,
                tint = if (task.important) Theme.text else Theme.ring,
            )
        }
    }
}

@Composable
private fun TaskDetails(task: Task, lists: List<TaskList>, update: ((Task) -> Task) -> Unit) {
    var picking by remember { mutableStateOf(false) }
    val external by SyncDocs.todo.external.collectAsState()
    Column(
        modifier = Modifier.padding(start = 44.dp, end = 12.dp, bottom = 10.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        EditField(
            external = external,
            current = { SyncDocs.todo.doc.value.tasks.firstOrNull { it.id == task.id }?.description },
            onChange = { text -> update { it.copy(description = text) } },
            placeholder = "Details",
            modifier = Modifier.fillMaxWidth(),
        )
        Row(modifier = Modifier.offset(x = (-12).dp), verticalAlignment = Alignment.CenterVertically) {
            SmallAction(lists.firstOrNull { it.id == task.list }?.name ?: "No list") { picking = true }
            SmallAction("Delete", danger = true) { SyncDocs.todo.edit { todo -> todo.discard { it.id == task.id } } }
        }
    }
    if (picking) {
        PickDialog(
            title = "Move to",
            options = listOf(NO_LIST) + lists.map { it.id },
            selected = task.list,
            name = { id -> if (id == NO_LIST) "No list" else lists.firstOrNull { it.id == id }?.name ?: "" },
            pick = { id -> update { it.copy(list = id) } },
            dismiss = { picking = false },
        )
    }
}

// Notes

private val DayFormat = DateTimeFormatter.ofPattern("MMM d", Locale.ENGLISH)

private fun shortDate(millis: Long): String {
    if (millis <= 0) return ""
    val day = Instant.ofEpochMilli(millis).atZone(ZoneId.systemDefault()).toLocalDate()
    return if (day == LocalDate.now()) "Today" else DayFormat.format(day)
}

@Composable
private fun NotesPage(close: () -> Unit) {
    val notes by SyncDocs.notes.doc.collectAsState()
    var opened by remember { mutableStateOf<Long?>(null) }
    val note = notes.firstOrNull { it.id == opened }

    if (note != null) {
        NoteEditor(note, back = { opened = null })
        return
    }

    ScreenColumn(scroll = false, spacing = 8.dp) {
        ScreenHeader(
            "Notes",
            if (notes.isEmpty()) null else "${notes.size} ${if (notes.size == 1) "note" else "notes"}",
            back = close,
        ) {
            IconTap(R.drawable.ic_plus, "New note", {
                val id = SyncDocs.newId()
                val now = System.currentTimeMillis()
                SyncDocs.notes.edit { list -> listOf(Note(id, "", "", now, now)) + list }
                opened = id
            })
        }
        LazyColumn(modifier = Modifier.weight(1f).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            if (notes.isEmpty()) item { Empty("No notes yet") }
            items(notes, key = { it.id }) { item ->
                ItemRow(onClick = { opened = item.id }) {
                    Row(
                        modifier = Modifier.padding(horizontal = 14.dp, vertical = 12.dp),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                            Text(
                                item.title.ifEmpty { "Untitled" },
                                fontSize = 15.sp,
                                fontWeight = FontWeight.Medium,
                                color = Theme.text,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Dim(
                                if (item.isPrivate == true) "Private" else item.body.replace(Regex("\\s+"), " ").trim().ifEmpty { "No text" },
                                maxLines = 2,
                            )
                        }
                        Dim(shortDate(item.updated), size = 12.sp)
                    }
                }
            }
        }
    }
}

@Composable
private fun NoteEditor(note: Note, back: () -> Unit) {
    BackHandler(onBack = back)
    val external by SyncDocs.notes.external.collectAsState()
    var title by remember(note.id) { mutableStateOf(note.title) }
    var body by remember(note.id) { mutableStateOf(note.body) }
    // A private note shows once asked for, not just because it was opened
    var revealed by remember(note.id) { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }

    // Only for changes from elsewhere: what's typed here can be ahead of what's been collected
    LaunchedEffect(external) {
        SyncDocs.notes.doc.value.firstOrNull { it.id == note.id }?.let {
            title = it.title
            body = it.body
        }
    }

    fun save() {
        val (newTitle, newBody) = title to body
        val now = System.currentTimeMillis()
        // Most recently edited first, as in the shell
        SyncDocs.notes.edit { list ->
            val edited = list.firstOrNull { it.id == note.id }?.copy(title = newTitle, body = newBody, updated = now)
            if (edited == null) list else listOf(edited) + list.filterNot { it.id == note.id }
        }
    }

    val covered = note.isPrivate == true && !revealed

    ScreenColumn(scroll = false, spacing = 4.dp) {
        ScreenHeader("Note", shortDate(note.updated).let { if (it.isEmpty()) null else "Edited $it" }, back = back) {
            Chip("Private", note.isPrivate == true) {
                val value = note.isPrivate != true
                // Not an edit: it keeps its place and date
                SyncDocs.notes.edit { list -> list.map { if (it.id == note.id) it.copy(isPrivate = value) else it } }
            }
            IconTap(R.drawable.ic_trash, "Delete", { deleting = true }, iconSize = 18.dp, tint = Theme.danger)
        }

        if (covered) {
            Column(
                modifier = Modifier.weight(1f).fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
            ) {
                Glyph(R.drawable.ic_lock, size = 28.dp, tint = Theme.textDim)
                Text("This note is private", fontSize = 15.sp, color = Theme.text)
                LisButton("Show", { revealed = true })
            }
        } else {
            Field(
                value = title,
                onChange = {
                    title = it
                    save()
                },
                placeholder = "Title",
                fontSize = 20.sp,
                fontWeight = FontWeight.SemiBold,
                plain = true,
                padding = PaddingValues(horizontal = 4.dp, vertical = 8.dp),
                modifier = Modifier.fillMaxWidth(),
            )
            Field(
                value = body,
                onChange = {
                    body = it
                    save()
                },
                placeholder = "Write here",
                singleLine = false,
                fill = true,
                plain = true,
                padding = PaddingValues(horizontal = 4.dp, vertical = 4.dp),
                modifier = Modifier.weight(1f).fillMaxWidth(),
            )
        }
    }

    if (deleting) {
        ConfirmDialog("Delete this note?", "It's gone from the PC too, with no trash to bring it back from.", "Delete", confirm = {
            val id = note.id
            back()
            SyncDocs.notes.edit { list -> list.filterNot { it.id == id } }
        }, dismiss = { deleting = false })
    }
}

// Goals

@Composable
private fun GoalsPage(close: () -> Unit) {
    val goals by SyncDocs.goals.doc.collectAsState()
    var opened by remember { mutableStateOf<Long?>(null) }
    val sorted = goals.filterNot { it.done } + goals.filter { it.done }
    val done = goals.count { it.done }

    ScreenColumn(scroll = false, spacing = 8.dp) {
        ScreenHeader("Goals", if (goals.isEmpty()) null else "$done of ${goals.size} done", back = close)
        LazyColumn(modifier = Modifier.weight(1f).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            if (goals.isEmpty()) item { Empty("No goals yet") }
            items(sorted, key = { it.id }) { goal ->
                GoalRow(goal, opened == goal.id) { opened = if (opened == goal.id) null else goal.id }
            }
        }
        AddField("New goal", add = { title ->
            val id = SyncDocs.newId()
            val now = System.currentTimeMillis()
            SyncDocs.goals.edit { list -> list + Goal(id, title, created = now) }
            opened = id
        }, modifier = Modifier.padding(bottom = 8.dp))
    }
}

private fun editGoal(id: Long, change: (Goal) -> Goal) {
    SyncDocs.goals.edit { list -> list.map { if (it.id == id) change(it) else it } }
}

@Composable
private fun GoalRow(goal: Goal, open: Boolean, toggleOpen: () -> Unit) {
    var deleting by remember { mutableStateOf(false) }
    val external by SyncDocs.goals.external.collectAsState()
    val finished = goal.subgoals.count { it.done }

    ItemRow(
        open = open,
        onClick = toggleOpen,
        details = {
            if (open) {
                Column(
                    modifier = Modifier.padding(start = 44.dp, end = 12.dp, bottom = 10.dp),
                    verticalArrangement = Arrangement.spacedBy(2.dp),
                ) {
                    EditField(
                        external = external,
                        current = { SyncDocs.goals.doc.value.firstOrNull { it.id == goal.id }?.title },
                        onChange = { text -> editGoal(goal.id) { it.copy(title = text.replace(Regex("[\\r\\n]+"), " ")) } },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Spacer(Modifier.height(4.dp))
                    goal.subgoals.forEach { subgoal -> SubgoalRow(goal.id, subgoal) }
                    Spacer(Modifier.height(4.dp))
                    AddField("New step", add = { title ->
                        val id = SyncDocs.newId()
                        editGoal(goal.id) { it.copy(subgoals = it.subgoals + Subgoal(id, title)) }
                    })
                    Box(modifier = Modifier.offset(x = (-12).dp)) {
                        SmallAction("Delete goal", danger = true) { deleting = true }
                    }
                }
            }
        },
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.heightIn(min = 52.dp)) {
            Check(goal.done, { done -> editGoal(goal.id) { it.copy(done = done) } })
            Column(modifier = Modifier.weight(1f).padding(vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    goal.title,
                    fontSize = 15.sp,
                    textDecoration = if (goal.done) TextDecoration.LineThrough else null,
                    color = if (goal.done) Theme.textDim else Theme.text,
                )
                if (goal.subgoals.isNotEmpty()) {
                    Box(Modifier.fillMaxWidth(0.6f).height(2.dp).clip(CircleShape).background(Theme.track)) {
                        Box(
                            Modifier
                                .fillMaxWidth(finished.toFloat() / goal.subgoals.size)
                                .fillMaxHeight()
                                .clip(CircleShape)
                                .background(if (goal.done) Theme.textDim else Theme.fill),
                        )
                    }
                }
            }
            if (goal.subgoals.isNotEmpty()) Dim("$finished/${goal.subgoals.size}", modifier = Modifier.padding(horizontal = 14.dp))
        }
    }

    if (deleting) {
        ConfirmDialog("Delete this goal?", "It's gone from the PC too, with no trash to bring it back from.", "Delete", confirm = {
            SyncDocs.goals.edit { list -> list.filterNot { it.id == goal.id } }
        }, dismiss = { deleting = false })
    }
}

@Composable
private fun SubgoalRow(goalId: Long, subgoal: Subgoal) {
    fun update(change: (Goal) -> Goal) = editGoal(goalId, change)

    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().offset(x = (-12).dp)) {
        Check(subgoal.done, { done ->
            update { goal -> goal.copy(subgoals = goal.subgoals.map { if (it.id == subgoal.id) it.copy(done = done) else it }) }
        }, size = 16.dp)
        Text(
            subgoal.title,
            fontSize = 14.sp,
            textDecoration = if (subgoal.done) TextDecoration.LineThrough else null,
            color = if (subgoal.done) Theme.textDim else Theme.text,
            modifier = Modifier.weight(1f),
        )
        IconTap(R.drawable.ic_close, "Remove", {
            update { goal -> goal.copy(subgoals = goal.subgoals.filterNot { it.id == subgoal.id }) }
        }, size = 36.dp, iconSize = 14.dp, tint = Theme.faint)
    }
}

// Calendar

private val MonthFormat = DateTimeFormatter.ofPattern("MMMM yyyy", Locale.ENGLISH)
private val LongDayFormat = DateTimeFormatter.ofPattern("EEEE, MMMM d", Locale.ENGLISH)
private val UpcomingFormat = DateTimeFormatter.ofPattern("EEE, MMM d", Locale.ENGLISH)
private val RepeatNames = mapOf("weekly" to "Weekly", "monthly" to "Monthly", "yearly" to "Yearly")

private fun nextRepeat(repeat: String): String =
    CalendarNote.REPEATS[(CalendarNote.REPEATS.indexOf(repeat) + 1) % CalendarNote.REPEATS.size]

/** The system's time picker; "No time" makes it all day. */
private fun pickTime(context: Context, current: String, set: (String) -> Unit) {
    val parts = current.split(":").mapNotNull { it.toIntOrNull() }
    val (hour, minute) = if (parts.size == 2) parts[0] to parts[1] else LocalTime.now().hour + 1 to 0
    val dialog = TimePickerDialog(context, { _, h, m -> set(String.format(Locale.ROOT, "%02d:%02d", h, m)) }, hour % 24, minute, true)
    dialog.setButton(DialogInterface.BUTTON_NEUTRAL, "No time") { _, _ -> set("") }
    dialog.show()
}
private const val UPCOMING_DAYS = 30L
private const val SWIPE_PX = 120f

@Composable
private fun CalendarPage(close: () -> Unit) {
    val notes by SyncDocs.calendar.doc.collectAsState()
    val settings by Prefs.settings.collectAsState()
    val today = LocalDate.now()
    var month by remember { mutableStateOf(YearMonth.from(today)) }
    var selected by remember { mutableStateOf(today) }
    var repeat by remember { mutableStateOf("none") }
    var time by remember { mutableStateOf("") }
    val context = LocalContext.current

    val firstDay = if (settings.weekStartsMonday) DayOfWeek.MONDAY else DayOfWeek.SUNDAY
    val start = month.atDay(1).let { first ->
        first.minusDays(((first.dayOfWeek.value - firstDay.value + 7) % 7).toLong())
    }
    val cells = (0 until 42).map { start.plusDays(it.toLong()) }
    val dayNotes = notes.filter { it.fallsOn(selected) }.sortedBy { it.time }
    val upcoming = (1..UPCOMING_DAYS).flatMap { offset ->
        val day = today.plusDays(offset)
        notes.filter { it.fallsOn(day) }.sortedBy { it.time }.map { day to it }
    }.take(6)

    fun select(day: LocalDate) {
        selected = day
        month = YearMonth.from(day)
    }

    ScreenColumn(spacing = 8.dp) {
        ScreenHeader("Calendar", if (notes.isEmpty()) null else "${notes.size} ${if (notes.size == 1) "note" else "notes"}", back = close)

        Row(verticalAlignment = Alignment.CenterVertically) {
            IconTap(R.drawable.ic_back, "Previous month", { month = month.minusMonths(1) }, tint = Theme.faint)
            Text(
                MonthFormat.format(month),
                fontSize = 16.sp,
                fontWeight = FontWeight.Medium,
                color = Theme.text,
                textAlign = TextAlign.Center,
                modifier = Modifier.weight(1f),
            )
            if (month != YearMonth.from(today) || selected != today) {
                LisButton("Today", { select(today) })
            }
            IconTap(R.drawable.ic_forward, "Next month", { month = month.plusMonths(1) }, tint = Theme.faint)
        }

        Column(
            modifier = Modifier.pointerInput(Unit) {
                var travel = 0f
                detectHorizontalDragGestures(
                    onDragStart = { travel = 0f },
                    onDragEnd = {
                        if (travel > SWIPE_PX) month = month.minusMonths(1)
                        if (travel < -SWIPE_PX) month = month.plusMonths(1)
                    },
                    onHorizontalDrag = { _, amount -> travel += amount },
                )
            },
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                (0 until 7).forEach { index ->
                    Text(
                        firstDay.plus(index.toLong()).getDisplayName(java.time.format.TextStyle.SHORT, Locale.ENGLISH),
                        fontSize = 12.sp,
                        color = Theme.textDim,
                        textAlign = TextAlign.Center,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
            cells.chunked(7).forEach { week ->
                Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    week.forEach { day ->
                        DayCell(
                            day = day,
                            inMonth = YearMonth.from(day) == month,
                            today = day == today,
                            selected = day == selected,
                            marked = notes.any { it.fallsOn(day) },
                            modifier = Modifier.weight(1f),
                        ) { select(day) }
                    }
                }
            }
        }

        SectionHeader(LongDayFormat.format(selected), start = 4.dp, top = 12.dp)
        if (dayNotes.isEmpty()) {
            StatusLine("No notes for this day")
        }
        dayNotes.forEach { note -> CalendarNoteRow(note) }

        AddField("Add a note…", add = { text ->
            val id = SyncDocs.newId()
            val note = CalendarNote(id, selected.toString(), text, repeat, time)
            SyncDocs.calendar.edit { list -> list + note }
            repeat = "none"
            time = ""
        }) {
            Chip(time.ifEmpty { "Time" }, time.isNotEmpty()) { pickTime(context, time) { time = it } }
            Chip(RepeatNames[repeat] ?: "Repeat", repeat != "none") { repeat = nextRepeat(repeat) }
        }

        if (upcoming.isNotEmpty()) {
            SectionHeader("Coming up", start = 4.dp, top = 16.dp)
            upcoming.forEach { (day, note) ->
                ItemRow(onClick = { select(day) }) {
                    Row(
                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 12.dp),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Text(UpcomingFormat.format(day), fontSize = 13.sp, color = Theme.textDim, modifier = Modifier.width(96.dp))
                        Text(
                            if (note.time.isEmpty()) note.text else "${note.time}  ${note.text}",
                            fontSize = 14.sp,
                            color = Theme.text,
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
            }
        }
        Spacer(Modifier.height(8.dp))
    }
}

/** As in the shell: today in the text color, the chosen day a step lighter, a dot under days with notes. */
@Composable
private fun DayCell(
    day: LocalDate,
    inMonth: Boolean,
    today: Boolean,
    selected: Boolean,
    marked: Boolean,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    Box(
        modifier = modifier
            .height(46.dp)
            .clip(RowShape)
            .background(
                when {
                    today -> Theme.text
                    selected -> Theme.border
                    pressed -> Theme.hover
                    else -> Color.Transparent
                },
            )
            .clickable(interactionSource = source, indication = null, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            "${day.dayOfMonth}",
            fontSize = 14.sp,
            fontWeight = if (today) FontWeight.SemiBold else FontWeight.Normal,
            color = when {
                today -> Theme.panel
                inMonth -> Theme.text
                else -> Theme.ring
            },
        )
        if (marked) {
            Box(
                Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = 6.dp)
                    .size(4.dp)
                    .clip(CircleShape)
                    .background(
                        when {
                            today -> Theme.panel
                            inMonth -> Theme.faint
                            else -> Theme.ring
                        },
                    ),
            )
        }
    }
}

@Composable
private fun CalendarNoteRow(note: CalendarNote) {
    var editing by remember { mutableStateOf(false) }
    val context = LocalContext.current
    val update: ((CalendarNote) -> CalendarNote) -> Unit = { change ->
        SyncDocs.calendar.edit { list -> list.map { if (it.id == note.id) change(it) else it } }
    }
    Row(
        modifier = Modifier.fillMaxWidth().clip(RowShape).background(Theme.hover).padding(start = 14.dp, end = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(6.dp).clip(CircleShape).background(Theme.faint))
        Text(
            note.text,
            fontSize = 15.sp,
            color = Theme.text,
            modifier = Modifier
                .weight(1f)
                .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null) { editing = true }
                .padding(horizontal = 12.dp, vertical = 12.dp),
        )
        Chip(note.time.ifEmpty { "Time" }, note.time.isNotEmpty()) {
            pickTime(context, note.time) { time -> update { it.copy(time = time) } }
        }
        Chip(RepeatNames[note.repeat] ?: "Repeat", note.repeat != "none") {
            update { it.copy(repeat = nextRepeat(it.repeat)) }
        }
        IconTap(R.drawable.ic_close, "Remove", {
            SyncDocs.calendar.edit { list -> list.filterNot { it.id == note.id } }
        }, size = 40.dp, iconSize = 14.dp, tint = Theme.faint)
    }
    if (editing) {
        NameDialog("Edit note", note.text, save = { text ->
            val clean = text.replace(Regex("[\\r\\n]+"), " ")
            SyncDocs.calendar.edit { list -> list.map { if (it.id == note.id) it.copy(text = clean) else it } }
        }, dismiss = { editing = false })
    }
}
