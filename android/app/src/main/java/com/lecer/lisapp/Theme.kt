package com.lecer.lisapp

import androidx.compose.foundation.text.selection.LocalTextSelectionColors
import androidx.compose.foundation.text.selection.TextSelectionColors
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow

/** The shell's colors, written as their dark values, as in its common/Theme.qml. */
@Immutable
class LisColors(val light: Boolean) {
    private fun tone(dark: Long): Color = adapt(Color(dark), light)

    val panel = tone(0xFF0F0F0F)
    val hover = tone(0xFF161616)
    val border = tone(0xFF1F1F1F)
    val raised = tone(0xFF262626)
    val track = tone(0xFF2A2A2A)
    val pressed = tone(0xFF333333)
    val ring = tone(0xFF4A4A4A)
    val textDim = tone(0xFF6B6B6B)
    val faint = tone(0xFF808080)
    val value = tone(0xFF999999)
    val fill = tone(0xFFBBBBBB)
    val text = tone(0xFFE6E6E6)
    val strong = tone(0xFFFFFFFF)
    val danger = tone(0xFFE06C6C)
}

private val LocalColors = staticCompositionLocalOf { LisColors(light = false) }

/** Read as in the shell: `Theme.panel`, `Theme.text`, `Theme.textDim`. */
object Theme {
    val light: Boolean @Composable @ReadOnlyComposable get() = LocalColors.current.light
    val panel: Color @Composable @ReadOnlyComposable get() = LocalColors.current.panel
    val hover: Color @Composable @ReadOnlyComposable get() = LocalColors.current.hover
    val border: Color @Composable @ReadOnlyComposable get() = LocalColors.current.border
    val raised: Color @Composable @ReadOnlyComposable get() = LocalColors.current.raised
    val track: Color @Composable @ReadOnlyComposable get() = LocalColors.current.track
    val pressed: Color @Composable @ReadOnlyComposable get() = LocalColors.current.pressed
    val ring: Color @Composable @ReadOnlyComposable get() = LocalColors.current.ring
    val textDim: Color @Composable @ReadOnlyComposable get() = LocalColors.current.textDim
    val faint: Color @Composable @ReadOnlyComposable get() = LocalColors.current.faint
    val value: Color @Composable @ReadOnlyComposable get() = LocalColors.current.value
    val fill: Color @Composable @ReadOnlyComposable get() = LocalColors.current.fill
    val text: Color @Composable @ReadOnlyComposable get() = LocalColors.current.text
    val strong: Color @Composable @ReadOnlyComposable get() = LocalColors.current.strong
    val danger: Color @Composable @ReadOnlyComposable get() = LocalColors.current.danger
}

private fun channel(value: Float): Double =
    if (value <= 0.04045f) value / 12.92 else ((value + 0.055) / 1.055).pow(2.4)

private fun luminance(color: Color): Double =
    0.2126 * channel(color.red) + 0.7152 * channel(color.green) + 0.0722 * channel(color.blue)

private val DARK_PANEL = luminance(Color(0xFF0F0F0F))
private val LIGHT_PANEL = luminance(Color(0xFFF5F5F5))

/** The shell's Theme.adapt: light mode keeps each color's contrast against the panel. */
fun adapt(color: Color, light: Boolean): Color {
    if (!light) return color

    val y = luminance(color)
    val above = y >= DARK_PANEL
    var ratio = if (above) (y + 0.05) / (DARK_PANEL + 0.05) else (DARK_PANEL + 0.05) / (y + 0.05)
    val high = max(color.red, max(color.green, color.blue))
    val low = min(color.red, min(color.green, color.blue))
    // A strong color turns muddy at text contrast
    if (high - low > 0.2f) ratio = min(ratio, 4.5)
    val target = if (above) (LIGHT_PANEL + 0.05) / ratio - 0.05 else min(1.0, (LIGHT_PANEL + 0.05) * ratio - 0.05)

    val delta = high - low
    val lightness = (high + low) / 2
    val saturation = if (delta == 0f) 0f else (delta / (1 - abs(2 * lightness - 1))).coerceIn(0f, 1f)
    val hue = when {
        delta == 0f -> 0f
        high == color.red -> 60 * ((color.green - color.blue) / delta).mod(6f)
        high == color.green -> 60 * ((color.blue - color.red) / delta + 2)
        else -> 60 * ((color.red - color.green) / delta + 4)
    }.coerceIn(0f, 360f)

    // Same hue and saturation, lightness searched for the target luminance
    var from = 0f
    var to = 1f
    var out = color
    repeat(16) {
        val middle = (from + to) / 2
        out = Color.hsl(hue, saturation, middle, color.alpha)
        if (luminance(out) < target) from = middle else to = middle
    }
    return out
}

enum class ThemeMode(val label: String) {
    SYSTEM("System"),
    DARK("Dark"),
    LIGHT("Light"),
    ;

    fun light(systemDark: Boolean): Boolean = when (this) {
        SYSTEM -> !systemDark
        DARK -> false
        LIGHT -> true
    }
}

@Composable
fun LisTheme(light: Boolean, content: @Composable () -> Unit) {
    val colors = remember(light) { LisColors(light) }
    val scheme = if (light) {
        lightColorScheme(
            primary = colors.text,
            onPrimary = colors.panel,
            background = colors.panel,
            onBackground = colors.text,
            surface = colors.panel,
            onSurface = colors.text,
            surfaceVariant = colors.hover,
            onSurfaceVariant = colors.textDim,
            surfaceContainer = colors.panel,
            surfaceContainerHigh = colors.panel,
            outline = colors.track,
            outlineVariant = colors.border,
            error = colors.danger,
        )
    } else {
        darkColorScheme(
            primary = colors.text,
            onPrimary = colors.panel,
            background = colors.panel,
            onBackground = colors.text,
            surface = colors.panel,
            onSurface = colors.text,
            surfaceVariant = colors.hover,
            onSurfaceVariant = colors.textDim,
            surfaceContainer = colors.panel,
            surfaceContainerHigh = colors.panel,
            outline = colors.track,
            outlineVariant = colors.border,
            error = colors.danger,
        )
    }
    MaterialTheme(colorScheme = scheme) {
        CompositionLocalProvider(
            LocalColors provides colors,
            LocalContentColor provides colors.text,
            LocalTextSelectionColors provides TextSelectionColors(handleColor = colors.text, backgroundColor = colors.pressed),
            content = content,
        )
    }
}
