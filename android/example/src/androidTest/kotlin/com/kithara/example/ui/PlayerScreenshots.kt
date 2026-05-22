package com.kithara.example.ui

import android.graphics.Bitmap
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import com.kithara.example.MainActivity
import java.io.File
import java.io.FileOutputStream
import org.junit.Rule
import org.junit.Test

/**
 * Drives the demo through each tab so an agent / CI run can compare
 * the Android UI against the iOS reference. Screenshots land in the
 * app's `filesDir/screenshots/` and can be pulled with `adb pull`.
 */
class PlayerScreenshots {
    @get:Rule
    val composeRule = createAndroidComposeRule<MainActivity>()

    @Test
    fun capturePlaylistTab() {
        // Default tab — wait for the Kithara header to confirm the
        // composition has settled before snapping a frame.
        composeRule.onNodeWithText("Kithara").assertExists()
        capture("android-playlist")
    }

    @Test
    fun captureEqTab() {
        composeRule.onNodeWithText("Kithara").assertExists()
        composeRule.onNodeWithText("EQ").performClick()
        composeRule.waitForIdle()
        capture("android-eq")
    }

    @Test
    fun captureSettingsTab() {
        composeRule.onNodeWithText("Kithara").assertExists()
        composeRule.onNodeWithText("Settings").performClick()
        composeRule.waitForIdle()
        capture("android-settings")
    }

    private fun capture(name: String) {
        composeRule.waitForIdle()
        val bitmap: Bitmap = composeRule.onRoot().captureToImage().asAndroidBitmap()
        val ctx = InstrumentationRegistry.getInstrumentation().targetContext
        val dir = File(ctx.filesDir, "screenshots").apply { mkdirs() }
        FileOutputStream(File(dir, "$name.png")).use { stream ->
            bitmap.compress(Bitmap.CompressFormat.PNG, 100, stream)
        }
    }
}
