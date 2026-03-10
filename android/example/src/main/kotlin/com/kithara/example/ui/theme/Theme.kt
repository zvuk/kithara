package com.kithara.example.ui.theme

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable

private val KitharaColorScheme = darkColorScheme(
    background = KitharaBackground,
    error = KitharaDanger,
    onBackground = PrimaryText,
    onError = PrimaryText,
    onPrimary = KitharaBackground,
    onSecondary = PrimaryText,
    onSurface = PrimaryText,
    primary = AccentGold,
    secondary = PanelBackground,
    surface = CardBackground,
)

@Composable
internal fun KitharaTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = KitharaColorScheme,
        content = content,
    )
}
