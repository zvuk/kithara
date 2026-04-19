package com.kithara

import android.content.Context
import android.util.Log
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Diagnostic test for Android-only audio distortion.
 *
 * Copies `test.mp3` out of the instrumentation APK assets into the app's
 * private `filesDir`, then asks the native library to render a fixed
 * number of seconds through the offline firewheel backend and write an
 * IEEE-float WAV to the app's external files directory (so it can be
 * fetched with `adb pull`).
 *
 * A clean output WAV means the decoder / graph compiled for Android is
 * producing correct samples — the distortion we hear on-device comes
 * from the output path (cpal / AAudio). A distorted output WAV means
 * the problem is upstream of the audio backend.
 *
 * Note: [Kithara.Test.runOfflineCapture] installs a process-wide offline
 * session client. Keep this test in its own class / run it in isolation
 * so that other tests in the same process do not clash.
 */
@RunWith(AndroidJUnit4::class)
class OfflineCaptureTest {

    companion object {
        private const val TAG = "OfflineCaptureTest"
        private const val INPUT_ASSET = "test.mp3"
        private const val OUTPUT_NAME = "offline-capture.wav"
        private const val CAPTURE_SECONDS = 10
        private const val WAV_HEADER_BYTES = 44L

        @JvmStatic
        @BeforeClass
        fun setUpClass() {
            val context = ApplicationProvider.getApplicationContext<Context>()
            Kithara.initialize(context, logLevel = LogLevel.Debug)
        }
    }

    @Test
    fun rendersCleanWav() {
        val appContext = ApplicationProvider.getApplicationContext<Context>()
        val testContext = InstrumentationRegistry.getInstrumentation().context

        val inputFile = File(appContext.filesDir, INPUT_ASSET)
        testContext.assets.open(INPUT_ASSET).use { input ->
            inputFile.outputStream().use { output -> input.copyTo(output) }
        }
        assertTrue("input copy must not be empty", inputFile.length() > 0)

        val outputDir = appContext.getExternalFilesDir(null)
            ?: appContext.filesDir
        val outputFile = File(outputDir, OUTPUT_NAME).apply {
            if (exists()) delete()
        }

        Log.i(TAG, "input=${inputFile.absolutePath}")
        Log.i(TAG, "output=${outputFile.absolutePath}")

        val rc = Kithara.Test.runOfflineCapture(
            inputPath = inputFile.absolutePath,
            outputPath = outputFile.absolutePath,
            seconds = CAPTURE_SECONDS,
        )

        assertEquals("native rc must be 0 (see RC_* constants in android_test.rs)", 0L, rc)
        assertTrue("output WAV must exist", outputFile.exists())
        assertTrue(
            "output WAV must contain samples (>${WAV_HEADER_BYTES}B); got ${outputFile.length()}",
            outputFile.length() > WAV_HEADER_BYTES,
        )

        Log.i(TAG, "captured ${outputFile.length()} bytes → pull with: adb pull ${outputFile.absolutePath}")
    }
}
