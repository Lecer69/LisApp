package com.lecer.lisapp

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val Dark = darkColorScheme(
    primary = Color(0xFFE6E6E6),
    onPrimary = Color(0xFF0F0F0F),
    secondaryContainer = Color(0xFF1F1F1F),
    onSecondaryContainer = Color(0xFFE6E6E6),
    background = Color(0xFF0F0F0F),
    onBackground = Color(0xFFE6E6E6),
    surface = Color(0xFF0F0F0F),
    onSurface = Color(0xFFE6E6E6),
    surfaceVariant = Color(0xFF161616),
    onSurfaceVariant = Color(0xFF8A8A8A),
    surfaceContainerHigh = Color(0xFF161616),
    outline = Color(0xFF2A2A2A),
    outlineVariant = Color(0xFF1F1F1F),
    error = Color(0xFFE06C6C),
)

private val Light = lightColorScheme(
    primary = Color(0xFF1A1A1A),
    onPrimary = Color(0xFFF7F7F7),
    secondaryContainer = Color(0xFFE6E6E6),
    onSecondaryContainer = Color(0xFF1A1A1A),
    background = Color(0xFFF7F7F7),
    onBackground = Color(0xFF1A1A1A),
    surface = Color(0xFFF7F7F7),
    onSurface = Color(0xFF1A1A1A),
    surfaceVariant = Color(0xFFEDEDED),
    onSurfaceVariant = Color(0xFF6B6B6B),
    surfaceContainerHigh = Color(0xFFEDEDED),
    outline = Color(0xFFD0D0D0),
    outlineVariant = Color(0xFFE0E0E0),
    error = Color(0xFFB3261E),
)

@Composable
fun LisTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = if (isSystemInDarkTheme()) Dark else Light, content = content)
}
