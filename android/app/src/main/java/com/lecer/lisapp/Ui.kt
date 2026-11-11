package com.lecer.lisapp

import androidx.annotation.DrawableRes
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog

// The shell's controls, sized for a finger. Pressing shows what hovering does in the shell: a step
// lighter, no ripple.

private val Small = RoundedCornerShape(8.dp)
private val Large = RoundedCornerShape(12.dp)

@Composable
private fun pressSource(): Pair<MutableInteractionSource, Boolean> {
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    return source to pressed
}

/** A whole screen: the panel color, clear of the system bars. */
@Composable
fun ScreenColumn(
    scroll: Boolean = true,
    spacing: Dp = 12.dp,
    content: @Composable ColumnScope.() -> Unit,
) {
    Box(Modifier.fillMaxSize().background(Theme.panel)) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .safeDrawingPadding()
                .let { if (scroll) it.verticalScroll(rememberScrollState()) else it }
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(spacing),
            content = content,
        )
    }
}

@Composable
fun Glyph(@DrawableRes id: Int, modifier: Modifier = Modifier, size: Dp = 20.dp, tint: Color = Theme.text, description: String? = null) {
    Image(
        painter = painterResource(id),
        contentDescription = description,
        modifier = modifier.size(size),
        colorFilter = ColorFilter.tint(tint),
    )
}

/** An icon on its own, like the shell's ✕ and arrows. */
@Composable
fun IconTap(
    @DrawableRes id: Int,
    description: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    size: Dp = 44.dp,
    iconSize: Dp = 20.dp,
    tint: Color = Theme.text,
    dot: Boolean = false,
) {
    val (source, pressed) = pressSource()
    Box(
        modifier = modifier
            .size(size)
            .alpha(if (enabled) 1f else 0.4f)
            .clip(Small)
            .background(if (pressed) Theme.hover else Color.Transparent)
            .clickable(interactionSource = source, indication = null, enabled = enabled, role = Role.Button, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Glyph(id, size = iconSize, tint = tint, description = description)
        if (dot) {
            Box(
                Modifier
                    .align(Alignment.TopEnd)
                    .padding(top = 10.dp, end = 10.dp)
                    .size(7.dp)
                    .clip(CircleShape)
                    .background(Theme.danger),
            )
        }
    }
}

/** The shell's PanelButton. */
@Composable
fun LisButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    danger: Boolean = false,
    @DrawableRes icon: Int? = null,
) {
    val (source, pressed) = pressSource()
    Row(
        modifier = modifier
            .alpha(if (enabled) 1f else 0.4f)
            .clip(Small)
            .background(if (pressed) Theme.pressed else Theme.border)
            .clickable(interactionSource = source, indication = null, enabled = enabled, role = Role.Button, onClick = onClick)
            .heightIn(min = 42.dp)
            .padding(horizontal = 16.dp),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val color = if (danger) Theme.danger else Theme.text
        icon?.let {
            Glyph(it, size = 16.dp, tint = color)
            Spacer(Modifier.width(8.dp))
        }
        Text(text, fontSize = 14.sp, fontWeight = FontWeight.Medium, color = color, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/** A square button with an icon over its name, for the main things to do. */
@Composable
fun Tile(
    @DrawableRes icon: Int,
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    detail: String? = null,
    enabled: Boolean = true,
) {
    val (source, pressed) = pressSource()
    Column(
        modifier = modifier
            .alpha(if (enabled) 1f else 0.4f)
            .clip(Large)
            .background(if (pressed) Theme.track else Theme.border)
            .clickable(interactionSource = source, indication = null, enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Glyph(icon, size = 22.dp)
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(label, fontSize = 14.sp, fontWeight = FontWeight.Medium, color = Theme.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (detail != null) Text(detail, fontSize = 12.sp, color = Theme.textDim, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** The shell's PanelFrame: a bordered panel with a title, and its subtitle beside it. */
@Composable
fun Panel(
    modifier: Modifier = Modifier,
    title: String? = null,
    subtitle: String? = null,
    trailing: @Composable (RowScope.() -> Unit)? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(Large)
            .background(Theme.panel)
            .border(1.dp, Theme.border, Large)
            .padding(horizontal = 18.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        if (title != null) {
            Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Row(modifier = Modifier.weight(1f)) {
                    Text(
                        title,
                        fontSize = 16.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = Theme.strong,
                        maxLines = 1,
                        modifier = Modifier.alignByBaseline(),
                    )
                    if (!subtitle.isNullOrEmpty()) {
                        Text(
                            subtitle,
                            fontSize = 13.sp,
                            color = Theme.textDim,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.alignByBaseline().padding(start = 8.dp),
                        )
                    }
                }
                trailing?.invoke(this)
            }
        }
        content()
    }
}

/** The top of a screen: back, the title with its subtitle under it, and icons on the right. */
@Composable
fun ScreenHeader(
    title: String,
    subtitle: String? = null,
    back: (() -> Unit)? = null,
    actions: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier = Modifier.fillMaxWidth().heightIn(min = 60.dp).padding(bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (back != null) {
            IconTap(R.drawable.ic_back, "Back", back, modifier = Modifier.offset(x = (-10).dp))
        }
        Column(modifier = Modifier.weight(1f).padding(start = if (back != null) 0.dp else 4.dp)) {
            Text(title, fontSize = 22.sp, fontWeight = FontWeight.SemiBold, color = Theme.strong, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (!subtitle.isNullOrEmpty()) {
                Text(subtitle, fontSize = 13.sp, color = Theme.textDim, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        actions()
    }
}

/** The shell's SectionHeader. */
@Composable
fun SectionHeader(title: String, modifier: Modifier = Modifier, start: Dp = 16.dp, top: Dp = 18.dp) {
    Text(
        title,
        fontSize = 13.sp,
        fontWeight = FontWeight.Medium,
        color = Theme.textDim,
        modifier = modifier.padding(start = start, top = top, bottom = 2.dp),
    )
}

@Composable
fun Dim(text: String, modifier: Modifier = Modifier, maxLines: Int = Int.MAX_VALUE, size: TextUnit = 13.sp, color: Color = Theme.textDim) {
    Text(text, fontSize = size, color = color, maxLines = maxLines, overflow = TextOverflow.Ellipsis, modifier = modifier)
}

/** The shell's SettingRow: a label and description, with its control on the right. */
@Composable
fun SettingRow(
    label: String,
    description: String? = null,
    onClick: (() -> Unit)? = null,
    enabled: Boolean = true,
    labelColor: Color = Theme.text,
    control: @Composable RowScope.() -> Unit = {},
) {
    val (source, pressed) = pressSource()
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(Small)
            .background(if (pressed) Theme.hover else Color.Transparent)
            .let {
                if (onClick != null) it.clickable(interactionSource = source, indication = null, enabled = enabled, onClick = onClick) else it
            }
            .alpha(if (enabled) 1f else 0.4f)
            .heightIn(min = 56.dp)
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(label, fontSize = 15.sp, color = labelColor)
            if (!description.isNullOrEmpty()) Dim(description)
        }
        control()
    }
}

/** A setting that's a switch: the whole row flips it. */
@Composable
fun ToggleRow(label: String, description: String? = null, checked: Boolean, enabled: Boolean = true, onChange: (Boolean) -> Unit) {
    SettingRow(label, description, onClick = { onChange(!checked) }, enabled = enabled) {
        Toggle(checked, onChange, enabled)
    }
}

/** A setting that picks one of a few, with the chips under its label as a phone is narrow. */
@Composable
fun <T> ChoiceRow(
    label: String,
    description: String? = null,
    options: List<T>,
    selected: T?,
    name: (T) -> String,
    enabled: Boolean = true,
    pick: (T) -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().alpha(if (enabled) 1f else 0.4f).padding(start = 16.dp, end = 16.dp, top = 10.dp, bottom = 6.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(label, fontSize = 15.sp, color = Theme.text)
            if (!description.isNullOrEmpty()) Dim(description)
        }
        Chips(options, selected, name, enabled = enabled, pick = pick, modifier = Modifier.offset(x = (-12).dp))
    }
}

/** The shell's InfoRow. */
@Composable
fun InfoRow(label: String, value: String, modifier: Modifier = Modifier) {
    Row(modifier = modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(label, fontSize = 14.sp, fontWeight = FontWeight.Medium, color = Theme.textDim, modifier = Modifier.weight(1f), maxLines = 1)
        Text(value, fontSize = 14.sp, color = Theme.value, textAlign = TextAlign.End)
    }
}

/** The shell's ToggleSwitch. */
@Composable
fun Toggle(checked: Boolean, onChange: (Boolean) -> Unit, enabled: Boolean = true) {
    val (source, _) = pressSource()
    val knob by animateDpAsState(if (checked) 21.dp else 4.dp, animationSpec = tween(140), label = "knob")
    val track by animateColorAsState(if (checked) Theme.text else Theme.track, animationSpec = tween(140), label = "track")
    Box(
        modifier = Modifier
            .size(width = 44.dp, height = 26.dp)
            .clip(CircleShape)
            .background(track)
            .clickable(interactionSource = source, indication = null, enabled = enabled, role = Role.Switch) { onChange(!checked) },
    ) {
        Box(
            Modifier
                .align(Alignment.CenterStart)
                .offset(x = knob)
                .size(19.dp)
                .clip(CircleShape)
                .background(if (checked) Theme.panel else Theme.faint),
        )
    }
}

/** The shell's PanelSlider, 0 to 1. Sends `done` once let go. */
@Composable
fun LisSlider(value: Float, onChange: (Float) -> Unit, modifier: Modifier = Modifier, enabled: Boolean = true, done: () -> Unit = {}) {
    val move by rememberUpdatedState(onChange)
    val finish by rememberUpdatedState(done)
    var dragging by remember { mutableStateOf(false) }
    val ratio = value.coerceIn(0f, 1f)
    BoxWithConstraints(
        modifier = modifier
            .fillMaxWidth()
            .height(36.dp)
            .alpha(if (enabled) 1f else 0.4f)
            .pointerInput(enabled) {
                if (!enabled) return@pointerInput
                detectTapGestures { offset ->
                    move((offset.x / size.width).coerceIn(0f, 1f))
                    finish()
                }
            }
            .pointerInput(enabled) {
                if (!enabled) return@pointerInput
                detectHorizontalDragGestures(
                    onDragStart = { offset ->
                        dragging = true
                        move((offset.x / size.width).coerceIn(0f, 1f))
                    },
                    onDragEnd = {
                        dragging = false
                        finish()
                    },
                    onDragCancel = {
                        dragging = false
                        finish()
                    },
                    onHorizontalDrag = { pointer, _ ->
                        pointer.consume()
                        move((pointer.position.x / size.width).coerceIn(0f, 1f))
                    },
                )
            },
        contentAlignment = Alignment.CenterStart,
    ) {
        val knob = if (dragging) 18.dp else 16.dp
        Box(Modifier.fillMaxWidth().height(3.dp).clip(CircleShape).background(Theme.track))
        Box(Modifier.fillMaxWidth(ratio).height(3.dp).clip(CircleShape).background(if (dragging) Theme.strong else Theme.fill))
        Box(
            Modifier
                .offset(x = (maxWidth - knob) * ratio)
                .size(knob)
                .clip(CircleShape)
                .background(Theme.strong),
        )
    }
}

/** The shell's UsageMeter: a label and value over a thin bar. */
@Composable
fun Meter(label: String, value: Float, valueText: String, modifier: Modifier = Modifier) {
    val shown by animateFloatAsState(value.coerceIn(0f, 1f), animationSpec = tween(400), label = "meter")
    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        InfoRow(label, valueText)
        Box(Modifier.fillMaxWidth().height(3.dp).clip(CircleShape).background(Theme.track)) {
            Box(Modifier.fillMaxWidth(shown).fillMaxHeight().clip(CircleShape).background(Theme.fill))
        }
    }
}

/** The shell's ChipSelector. Scrolls sideways when a phone's width doesn't fit them all. */
@Composable
fun <T> Chips(
    options: List<T>,
    selected: T?,
    name: (T) -> String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    pick: (T) -> Unit,
) {
    Row(
        modifier = modifier.horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        options.forEach { option ->
            Chip(name(option), option == selected, enabled) { pick(option) }
        }
    }
}

@Composable
fun Chip(label: String, active: Boolean, enabled: Boolean = true, onClick: () -> Unit) {
    val (source, pressed) = pressSource()
    Box(
        modifier = Modifier
            .clip(Small)
            .background(
                when {
                    active && pressed -> Theme.pressed
                    active -> Theme.raised
                    pressed -> Theme.hover
                    else -> Color.Transparent
                },
            )
            .clickable(interactionSource = source, indication = null, enabled = enabled, role = Role.Tab, onClick = onClick)
            .heightIn(min = 36.dp)
            .padding(horizontal = 12.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, fontSize = 14.sp, color = if (active) Theme.strong else Theme.faint, maxLines = 1)
    }
}

/** The shell's round check, as on its todos and goals. */
@Composable
fun Check(checked: Boolean, onChange: (Boolean) -> Unit, modifier: Modifier = Modifier, size: Dp = 20.dp) {
    val (source, _) = pressSource()
    Box(
        modifier = modifier
            .size(44.dp)
            .clickable(interactionSource = source, indication = null, role = Role.Checkbox) { onChange(!checked) },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(size)
                .clip(CircleShape)
                .background(if (checked) Theme.text else Color.Transparent)
                .border(1.5.dp, if (checked) Theme.text else Theme.ring, CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            if (checked) Glyph(R.drawable.ic_check, size = size * 0.65f, tint = Theme.panel)
        }
    }
}

/** The shell's text boxes: no outline, a step lighter than the panel. */
@Composable
fun Field(
    value: String,
    onChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    placeholder: String = "",
    singleLine: Boolean = true,
    fontSize: TextUnit = 15.sp,
    fontWeight: FontWeight? = null,
    fill: Boolean = false,
    plain: Boolean = false,
    padding: PaddingValues = PaddingValues(horizontal = 14.dp, vertical = 12.dp),
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
    keyboardActions: KeyboardActions = KeyboardActions.Default,
) {
    BasicTextField(
        value = value,
        onValueChange = onChange,
        modifier = modifier,
        singleLine = singleLine,
        textStyle = TextStyle(color = Theme.text, fontSize = fontSize, fontWeight = fontWeight),
        cursorBrush = SolidColor(Theme.text),
        keyboardOptions = keyboardOptions,
        keyboardActions = keyboardActions,
        decorationBox = { inner ->
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .let { if (fill) it.fillMaxHeight() else it }
                    .clip(Small)
                    .background(if (plain) Color.Transparent else Theme.hover)
                    .heightIn(min = 46.dp)
                    .padding(padding),
                contentAlignment = if (singleLine) Alignment.CenterStart else Alignment.TopStart,
            ) {
                if (value.isEmpty()) Text(placeholder, fontSize = fontSize, fontWeight = fontWeight, color = Theme.textDim)
                inner()
            }
        },
    )
}

/** A text box with an add button, which also adds on the keyboard's Done. */
@Composable
fun AddField(hint: String, add: (String) -> Unit, modifier: Modifier = Modifier, extra: @Composable RowScope.() -> Unit = {}) {
    var text by remember { mutableStateOf("") }
    val submit: () -> Unit = {
        val clean = text.replace(Regex("[\\r\\n]+"), " ").trim()
        if (clean.isNotEmpty()) {
            add(clean)
            text = ""
        }
    }
    Row(
        modifier = modifier.fillMaxWidth().clip(Small).background(Theme.hover).padding(end = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Field(
            value = text,
            onChange = { text = it },
            placeholder = hint,
            plain = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { submit() }),
            modifier = Modifier.weight(1f),
        )
        extra()
        IconTap(R.drawable.ic_plus, "Add", submit, enabled = text.isNotBlank(), size = 40.dp)
    }
}

/** The shell's panels as a dialog. */
@Composable
fun PanelDialog(
    title: String,
    dismiss: () -> Unit,
    buttons: @Composable RowScope.() -> Unit,
    content: @Composable ColumnScope.() -> Unit = {},
) {
    Dialog(onDismissRequest = dismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(Large)
                .background(Theme.panel)
                .border(1.dp, Theme.border, Large)
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text(title, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, color = Theme.strong)
            content()
            Row(
                modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                content = buttons,
            )
        }
    }
}

@Composable
fun ConfirmDialog(title: String, text: String, action: String, confirm: () -> Unit, dismiss: () -> Unit) {
    PanelDialog(
        title = title,
        dismiss = dismiss,
        buttons = {
            LisButton("Cancel", dismiss)
            LisButton(action, {
                dismiss()
                confirm()
            }, danger = true)
        },
    ) {
        Dim(text, size = 14.sp)
    }
}

@Composable
fun NameDialog(title: String, initial: String, save: (String) -> Unit, dismiss: () -> Unit) {
    var name by remember { mutableStateOf(initial) }
    PanelDialog(
        title = title,
        dismiss = dismiss,
        buttons = {
            LisButton("Cancel", dismiss)
            LisButton("Save", {
                dismiss()
                save(name.trim())
            }, enabled = name.isNotBlank())
        },
    ) {
        Field(name, { name = it }, modifier = Modifier.fillMaxWidth())
    }
}

/** One of a few, as rows in a panel. */
@Composable
fun <T> PickDialog(title: String, options: List<T>, selected: T?, name: (T) -> String, pick: (T) -> Unit, dismiss: () -> Unit) {
    PanelDialog(title = title, dismiss = dismiss, buttons = { LisButton("Cancel", dismiss) }) {
        Column(modifier = Modifier.offset(x = (-8).dp)) {
            options.forEach { option ->
                val (source, pressed) = pressSource()
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(Small)
                        .background(if (pressed) Theme.hover else Color.Transparent)
                        .clickable(interactionSource = source, indication = null) {
                            dismiss()
                            pick(option)
                        }
                        .heightIn(min = 44.dp)
                        .padding(horizontal = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(name(option), fontSize = 15.sp, color = if (option == selected) Theme.strong else Theme.text, modifier = Modifier.weight(1f))
                    if (option == selected) Glyph(R.drawable.ic_check, size = 16.dp)
                }
            }
        }
    }
}

/** Short status lines, in the danger color when something went wrong. */
@Composable
fun StatusLine(text: String, error: Boolean = false, modifier: Modifier = Modifier) {
    Text(
        text,
        fontSize = 13.sp,
        color = if (error) Theme.danger else Theme.textDim,
        modifier = modifier.padding(horizontal = 4.dp),
    )
}
