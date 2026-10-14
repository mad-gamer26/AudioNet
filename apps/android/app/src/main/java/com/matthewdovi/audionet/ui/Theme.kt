package com.matthewdovi.audionet.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

/**
 * AudioNet's colours: the accent at WCAG AA contrast (4.5:1 or more) on
 * both light and dark backgrounds, as in the iPhone app (#0054BF light,
 * #73ADFF dark).
 */
@Composable
fun AudioNetTheme(content: @Composable () -> Unit) {
    val scheme = if (isSystemInDarkTheme()) {
        darkColorScheme(primary = Color(0xFF73ADFF), onPrimary = Color(0xFF00214D))
    } else {
        lightColorScheme(primary = Color(0xFF0054BF), onPrimary = Color.White)
    }
    MaterialTheme(colorScheme = scheme, content = content)
}
