package com.kithara

import android.content.Context
import android.util.Log
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.math.abs
import kotlin.math.sin
import kotlin.math.sqrt
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

/** Verifies PCM rendered by an independent offline Host. */
@RunWith(AndroidJUnit4::class)
class OfflineCaptureTest {

    companion object {
        private const val TAG = "OfflineCaptureTest"
        private const val INPUT_ASSET = "test.mp3"
        private const val OUTPUT_NAME = "offline-capture.wav"
        private const val CAPTURE_SECONDS = 10

        private const val HEADER_BYTES = 44
        private const val WAV_FORMAT_IEEE_FLOAT = 3
        private const val CHANNELS = 2
        private const val SAMPLE_RATE = 44_100
        private const val BITS_PER_SAMPLE = 32
        private const val BYTES_PER_SAMPLE = BITS_PER_SAMPLE / 8
        private const val BLOCK_ALIGN = CHANNELS * BYTES_PER_SAMPLE
        private const val BYTE_RATE = SAMPLE_RATE * BLOCK_ALIGN

        private const val FRAMES_BEFORE_MEASURING = 5_000
        private const val MIN_PEAK = 0.5f
        private const val MAX_CHANNEL_DRIFT = 0.05f
        // The fixture is a full-scale 440 Hz sine; MP3 decoding permits small error.
        private const val MAX_STEP = 0.1f
        private const val MIN_RMS = 0.6
        private const val MAX_RMS = 0.8
        private const val QUIET_SAMPLE = 0.0001f
        private const val MAX_QUIET_FRACTION = 0.01

        @JvmStatic
        @BeforeClass
        fun setUpClass() {
            val context = ApplicationProvider.getApplicationContext<Context>()
            Kithara.initialize(context, logLevel = LogLevel.Debug)
        }
    }

    @Test
    fun rendersCleanWav() {
        val input = copyFixtureIntoAppFiles()
        val output = emptyOutputFile()

        Log.i(TAG, "input=${input.absolutePath} output=${output.absolutePath}")

        val rc = Kithara.Test.runOfflineCapture(
            inputPath = input.absolutePath,
            outputPath = output.absolutePath,
            seconds = CAPTURE_SECONDS,
        )

        assertEquals("native rc must be 0 (see RC_* constants in android_test.rs)", 0L, rc)
        assertTrue("output WAV must exist", output.exists())

        val bytes = output.readBytes()
        val frames = assertHeader(bytes, output.length())
        assertWave(readPcm(bytes, frames))
    }

    private fun copyFixtureIntoAppFiles(): File {
        val appContext = ApplicationProvider.getApplicationContext<Context>()
        val testContext = InstrumentationRegistry.getInstrumentation().context
        val input = File(appContext.filesDir, INPUT_ASSET)

        testContext.assets.open(INPUT_ASSET).use { asset ->
            input.outputStream().use { file -> asset.copyTo(file) }
        }

        assertTrue("input copy must not be empty", input.length() > 0)
        return input
    }

    private fun emptyOutputFile(): File {
        val appContext = ApplicationProvider.getApplicationContext<Context>()
        val dir = appContext.getExternalFilesDir(null) ?: appContext.filesDir
        return File(dir, OUTPUT_NAME).apply { if (exists()) delete() }
    }

    private fun assertHeader(bytes: ByteArray, fileLength: Long): Int {
        assertTrue("WAV must carry a full header; got ${bytes.size} bytes", bytes.size >= HEADER_BYTES)
        val header = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)

        assertEquals("RIFF", tag(bytes, 0))
        assertEquals("WAVE", tag(bytes, 8))
        assertEquals("fmt ", tag(bytes, 12))
        assertEquals("fmt chunk size", 16, header.getInt(16))
        assertEquals("audio format must be IEEE float", WAV_FORMAT_IEEE_FLOAT, header.getShort(20).toInt())
        assertEquals("channels", CHANNELS, header.getShort(22).toInt())
        assertEquals("sample rate", SAMPLE_RATE, header.getInt(24))
        assertEquals("byte rate", BYTE_RATE, header.getInt(28))
        assertEquals("block align", BLOCK_ALIGN, header.getShort(32).toInt())
        assertEquals("bits per sample", BITS_PER_SAMPLE, header.getShort(34).toInt())
        assertEquals("data", tag(bytes, 36))

        val dataBytes = header.getInt(40)
        assertEquals("RIFF size must cover the data chunk", HEADER_BYTES - 8 + dataBytes, header.getInt(4))
        assertEquals("the file must hold exactly the declared data", (HEADER_BYTES + dataBytes).toLong(), fileLength)
        assertEquals("data must be a whole number of frames", 0, dataBytes % BLOCK_ALIGN)

        val frames = dataBytes / BLOCK_ALIGN
        assertEquals("capture must render ${CAPTURE_SECONDS}s", CAPTURE_SECONDS * SAMPLE_RATE, frames)
        return frames
    }

    private fun readPcm(bytes: ByteArray, frames: Int): FloatArray {
        val pcm = FloatArray(frames * CHANNELS)
        ByteBuffer.wrap(bytes, HEADER_BYTES, pcm.size * BYTES_PER_SAMPLE)
            .order(ByteOrder.LITTLE_ENDIAN)
            .asFloatBuffer()
            .get(pcm)
        return pcm
    }

    private fun assertWave(pcm: FloatArray) {
        val peak = peak(pcm)
        val channelDrift = channelDrift(pcm)
        var energy = 0.0
        var maxStep = 0f
        var quietFrames = 0
        val frames = pcm.size / CHANNELS
        for (sample in pcm) assertTrue("PCM must be finite", sample.isFinite())
        for (frame in FRAMES_BEFORE_MEASURING until frames) {
            val sample = pcm[frame * CHANNELS]
            energy += sample.toDouble() * sample
            if (abs(sample) < QUIET_SAMPLE) quietFrames++
            maxStep = maxOf(maxStep, abs(sample - pcm[(frame - 1) * CHANNELS]))
        }
        val measuredFrames = frames - FRAMES_BEFORE_MEASURING
        val rms = sqrt(energy / measuredFrames)
        val quietFraction = quietFrames.toDouble() / measuredFrames
        Log.i(TAG, "frames=$frames peak=$peak channelDrift=$channelDrift maxStep=$maxStep rms=$rms quietFraction=$quietFraction")
        assertTrue("captured audio must not be silent; peak=$peak", peak > MIN_PEAK)
        assertTrue("stereo sine channel drift=$channelDrift", channelDrift < MAX_CHANNEL_DRIFT)
        assertTrue("440 Hz fixture RMS=$rms", rms in MIN_RMS..MAX_RMS)
        assertTrue("440 Hz fixture step=$maxStep", maxStep < MAX_STEP)
        assertTrue("440 Hz fixture quiet fraction=$quietFraction", quietFraction < MAX_QUIET_FRACTION)
    }

    @Test
    fun pcmOracleRejectsShortReadZeroPadding() {
        val clean = FloatArray(SAMPLE_RATE * CHANNELS) { index ->
            (0.95 * sin(2.0 * Math.PI * 440.0 * (index / CHANNELS) / SAMPLE_RATE)).toFloat()
        }
        assertWave(clean)
        val corrupted = clean.clone()
        // The old capture consumed a sample count as frames, leaving half each block zeroed.
        for (frame in 0 until corrupted.size / CHANNELS) {
            if (frame % 256 >= 128) {
                corrupted[frame * CHANNELS] = 0f
                corrupted[frame * CHANNELS + 1] = 0f
            }
        }
        assertThrows(AssertionError::class.java) { assertWave(corrupted) }
    }

    private fun peak(pcm: FloatArray): Float {
        var peak = 0f
        for (sample in pcm) {
            peak = maxOf(peak, abs(sample))
        }
        return peak
    }

    private fun channelDrift(pcm: FloatArray): Float {
        var drift = 0f
        for (frame in 0 until pcm.size / CHANNELS) {
            drift = maxOf(drift, abs(pcm[frame * CHANNELS] - pcm[frame * CHANNELS + 1]))
        }
        return drift
    }

    private fun tag(bytes: ByteArray, at: Int): String =
        String(bytes, at, 4, Charsets.US_ASCII)
}
