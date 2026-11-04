package com.lecer.lisapp

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** The shell's own todos, notes and goals. They work without the PC, and catch up when it's back. */
enum class OrganizerPage(val title: String) {
    TODOS("Todos"),
    NOTES("Notes"),
    GOALS("Goals"),
}

@Composable
fun OrganizerScreen(page: OrganizerPage, close: () -> Unit) {
    Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        when (page) {
            OrganizerPage.TODOS -> TodosPage(close)
            OrganizerPage.NOTES -> NotesPage(close)
            OrganizerPage.GOALS -> GoalsPage(close)
        }
    }
}

@Composable
private fun PageColumn(content: @Composable ColumnScope.() -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize().safeDrawingPadding().padding(horizontal = 20.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        content()
    }
}

@Composable
private fun PageHeader(title: String, subtitle: String, close: () -> Unit) {
    Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title, fontSize = 24.sp, fontWeight = FontWeight.SemiBold)
            if (subtitle.isNotEmpty()) Dim(subtitle)
        }
        TextButton(onClick = close) { Text("Close") }
    }
}

@Composable
private fun Dim(text: String, maxLines: Int = Int.MAX_VALUE) {
    Text(
        text,
        fontSize = 13.sp,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = maxLines,
        overflow = TextOverflow.Ellipsis,
    )
}

/** A task or a goal is one line, as in the shell. */
private fun oneLine(text: String): String = text.replace(Regex("[\\r\\n]+"), " ").trim()

@Composable
private fun AddRow(hint: String, add: (String) -> Unit) {
    var text by remember { mutableStateOf("") }
    val submit: () -> Unit = {
        val clean = oneLine(text)
        if (clean.isNotEmpty()) {
            add(clean)
            text = ""
        }
    }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedTextField(
            value = text,
            onValueChange = { text = it },
            singleLine = true,
            placeholder = { Text(hint) },
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { submit() }),
            modifier = Modifier.weight(1f),
        )
        OutlinedButton(enabled = text.isNotBlank(), onClick = submit) { Text("Add") }
    }
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
    OutlinedTextField(
        value = text,
        onValueChange = {
            text = it
            onChange(it)
        },
        singleLine = singleLine,
        placeholder = { Text(placeholder) },
        modifier = modifier,
    )
}

@Composable
private fun ConfirmDialog(title: String, text: String, action: String, confirm: () -> Unit, dismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(title) },
        text = { Text(text) },
        confirmButton = {
            TextButton(onClick = {
                dismiss()
                confirm()
            }) { Text(action, color = MaterialTheme.colorScheme.error) }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text("Cancel") } },
    )
}

@Composable
private fun NameDialog(title: String, initial: String, save: (String) -> Unit, dismiss: () -> Unit) {
    var name by remember { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(title) },
        text = { OutlinedTextField(value = name, onValueChange = { name = it }, singleLine = true) },
        confirmButton = {
            TextButton(enabled = name.isNotBlank(), onClick = {
                dismiss()
                save(name.trim())
            }) { Text("Save") }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text("Cancel") } },
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

    PageColumn {
        PageHeader("Todos", if (shown.isEmpty()) "" else "$open open", close)

        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
        ) {
            FilterChip(selected = current == NO_LIST, onClick = { list = NO_LIST }, label = { Text("All") })
            doc.lists.forEach { item ->
                FilterChip(selected = current == item.id, onClick = { list = item.id }, label = { Text(item.name) })
            }
            FilterChip(selected = false, onClick = { naming = true }, label = { Text("New list") })
        }

        if (current != NO_LIST && current != DAILY_LIST) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton(onClick = { renaming = true }) { Text("Rename list") }
                TextButton(onClick = { removing = true }) { Text("Delete list", color = MaterialTheme.colorScheme.error) }
            }
        }

        LazyColumn(modifier = Modifier.weight(1f).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            items(shown, key = { it.id }) { task ->
                TaskRow(task, doc.lists, opened == task.id) { opened = if (opened == task.id) null else task.id }
            }
        }

        if (shown.any { it.done && it.list != DAILY_LIST }) {
            TextButton(onClick = {
                val cleared = current
                SyncDocs.todo.edit { todo ->
                    todo.discard { it.done && it.list != DAILY_LIST && (cleared == NO_LIST || it.list == cleared) }
                }
            }) { Text("Clear done") }
        }

        AddRow("New task") { text ->
            val id = SyncDocs.newId()
            val into = current
            SyncDocs.todo.edit { todo -> todo.copy(tasks = todo.tasks + Task(id, text, list = into)) }
        }
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
private fun TaskRow(task: Task, lists: List<TaskList>, open: Boolean, toggleOpen: () -> Unit) {
    fun update(change: (Task) -> Task) {
        SyncDocs.todo.edit { todo -> todo.copy(tasks = todo.tasks.map { if (it.id == task.id) change(it) else it }) }
    }

    Column {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().clickable(onClick = toggleOpen)) {
            Checkbox(checked = task.done, onCheckedChange = { done -> update { it.copy(done = done) } })
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    task.text,
                    fontSize = 15.sp,
                    textDecoration = if (task.done) TextDecoration.LineThrough else null,
                    color = if (task.done) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
                )
                if (!open && task.description.isNotEmpty()) Dim(task.description, maxLines = 1)
            }
            TextButton(onClick = { update { it.copy(important = !it.important) } }) {
                Text(if (task.important) "★" else "☆", fontSize = 18.sp)
            }
        }
        if (open) TaskDetails(task, lists, ::update)
    }
}

@Composable
private fun TaskDetails(task: Task, lists: List<TaskList>, update: ((Task) -> Task) -> Unit) {
    var menu by remember { mutableStateOf(false) }
    val external by SyncDocs.todo.external.collectAsState()
    Column(modifier = Modifier.padding(start = 48.dp, bottom = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        EditField(
            external = external,
            current = { SyncDocs.todo.doc.value.tasks.firstOrNull { it.id == task.id }?.description },
            onChange = { text -> update { it.copy(description = text) } },
            placeholder = "Details",
            modifier = Modifier.fillMaxWidth(),
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box {
                TextButton(onClick = { menu = true }) { Text(lists.firstOrNull { it.id == task.list }?.name ?: "No list") }
                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                    DropdownMenuItem(text = { Text("No list") }, onClick = {
                        menu = false
                        update { it.copy(list = NO_LIST) }
                    })
                    lists.forEach { list ->
                        DropdownMenuItem(text = { Text(list.name) }, onClick = {
                            menu = false
                            update { it.copy(list = list.id) }
                        })
                    }
                }
            }
            TextButton(onClick = { SyncDocs.todo.edit { todo -> todo.discard { it.id == task.id } } }) {
                Text("Delete", color = MaterialTheme.colorScheme.error)
            }
        }
    }
}

// Notes

@Composable
private fun NotesPage(close: () -> Unit) {
    val notes by SyncDocs.notes.doc.collectAsState()
    var opened by remember { mutableStateOf<Long?>(null) }
    val note = notes.firstOrNull { it.id == opened }

    if (note != null) {
        NoteEditor(note, back = { opened = null })
        return
    }

    PageColumn {
        PageHeader("Notes", if (notes.isEmpty()) "" else "${notes.size} ${if (notes.size == 1) "note" else "notes"}", close)
        LazyColumn(modifier = Modifier.weight(1f).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            items(notes, key = { it.id }) { item ->
                Column(modifier = Modifier.fillMaxWidth().clickable { opened = item.id }.padding(vertical = 10.dp)) {
                    Text(item.title.ifEmpty { "Untitled" }, fontSize = 15.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Dim(
                        if (item.isPrivate == true) "Private" else item.body.replace(Regex("\\s+"), " ").trim().ifEmpty { "No text" },
                        maxLines = 1,
                    )
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            }
        }
        OutlinedButton(modifier = Modifier.fillMaxWidth(), onClick = {
            val id = SyncDocs.newId()
            val now = System.currentTimeMillis()
            SyncDocs.notes.edit { list -> listOf(Note(id, "", "", now, now)) + list }
            opened = id
        }) { Text("New note") }
    }
}

@Composable
private fun NoteEditor(note: Note, back: () -> Unit) {
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

    PageColumn {
        Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            TextButton(onClick = back) { Text("Back") }
            Box(modifier = Modifier.weight(1f))
            TextButton(onClick = {
                val value = note.isPrivate != true
                // Not an edit: it keeps its place and date
                SyncDocs.notes.edit { list -> list.map { if (it.id == note.id) it.copy(isPrivate = value) else it } }
            }) { Text(if (note.isPrivate == true) "Private" else "Make private") }
            TextButton(onClick = { deleting = true }) { Text("Delete", color = MaterialTheme.colorScheme.error) }
        }

        if (covered) {
            Column(
                modifier = Modifier.weight(1f).fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                Text("This note is private", fontSize = 15.sp)
                TextButton(onClick = { revealed = true }) { Text("Show") }
            }
        } else {
            OutlinedTextField(
                value = title,
                onValueChange = {
                    title = it
                    save()
                },
                singleLine = true,
                placeholder = { Text("Title") },
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = body,
                onValueChange = {
                    body = it
                    save()
                },
                placeholder = { Text("Write here") },
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

    PageColumn {
        PageHeader("Goals", if (goals.isEmpty()) "" else "$done of ${goals.size} done", close)
        Column(
            modifier = Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            sorted.forEach { goal ->
                GoalRow(goal, opened == goal.id) { opened = if (opened == goal.id) null else goal.id }
            }
        }
        AddRow("New goal") { title ->
            val id = SyncDocs.newId()
            val now = System.currentTimeMillis()
            SyncDocs.goals.edit { list -> list + Goal(id, title, created = now) }
            opened = id
        }
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

    Column {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().clickable(onClick = toggleOpen)) {
            Checkbox(checked = goal.done, onCheckedChange = { done -> editGoal(goal.id) { it.copy(done = done) } })
            Text(
                goal.title,
                fontSize = 15.sp,
                textDecoration = if (goal.done) TextDecoration.LineThrough else null,
                modifier = Modifier.weight(1f),
            )
            if (goal.subgoals.isNotEmpty()) Dim("$finished/${goal.subgoals.size}")
        }
        if (open) {
            Column(modifier = Modifier.padding(start = 48.dp, bottom = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                EditField(
                    external = external,
                    current = { SyncDocs.goals.doc.value.firstOrNull { it.id == goal.id }?.title },
                    onChange = { text -> editGoal(goal.id) { it.copy(title = text) } },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                goal.subgoals.forEach { subgoal -> SubgoalRow(goal.id, subgoal) }
                AddRow("New step") { title ->
                    val id = SyncDocs.newId()
                    editGoal(goal.id) { it.copy(subgoals = it.subgoals + Subgoal(id, title)) }
                }
                TextButton(onClick = { deleting = true }) { Text("Delete goal", color = MaterialTheme.colorScheme.error) }
            }
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

    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
        Checkbox(checked = subgoal.done, onCheckedChange = { done ->
            update { goal -> goal.copy(subgoals = goal.subgoals.map { if (it.id == subgoal.id) it.copy(done = done) else it }) }
        })
        Text(
            subgoal.title,
            fontSize = 14.sp,
            textDecoration = if (subgoal.done) TextDecoration.LineThrough else null,
            modifier = Modifier.weight(1f),
        )
        TextButton(onClick = {
            update { goal -> goal.copy(subgoals = goal.subgoals.filterNot { it.id == subgoal.id }) }
        }) { Text("Remove", fontSize = 12.sp) }
    }
}
