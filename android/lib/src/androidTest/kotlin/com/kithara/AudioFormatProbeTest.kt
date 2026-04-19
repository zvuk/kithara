package com.kithara

import android.content.Context
import android.util.Log
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Probes cpal's default host / output device on Android and asserts the
 * default sample format matches what the firewheel graph produces.
 *
 * The graph output is interleaved stereo f32. If cpal negotiates a
 * different format (e.g. I16) with the Android backend, every frame is
 * converted inside cpal before it reaches AAudio — a likely source of
 * the on-device audio distortion we see in the app but not in
 * [OfflineCaptureTest]'s offline WAV.
 *
 * Read full supported-config list via `adb logcat -v brief | grep cpal`.
 */
@RunWith(AndroidJUnit4::class)
class AudioFormatProbeTest {

    companion object {
        private const val TAG = "AudioFormatProbeTest"

        @JvmStatic
        @BeforeClass
        fun setUpClass() {
            val context = ApplicationProvider.getApplicationContext<Context>()
            Kithara.initialize(context, logLevel = LogLevel.Debug)
        }
    }

    @Test
    fun defaultOutputFormatIsF32() {
        val code = Kithara.Test.probeAndroidAudio()
        val name = Kithara.Test.SampleFormat.name(code)
        Log.i(TAG, "cpal default output sample format: $name ($code)")

        assertTrue("probe must not return a negative error code; got $name", code >= 0)
        assertEquals(
            "cpal default output must be F32 on Android to match the firewheel graph output. " +
                "A different format ($name) means cpal converts every sample before sending to " +
                "AAudio — inspect `cpal supported output config` logs for alternatives.",
            Kithara.Test.SampleFormat.F32,
            code,
        )
    }
}
