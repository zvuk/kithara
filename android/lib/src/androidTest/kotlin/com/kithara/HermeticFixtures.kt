package com.kithara

/**
 * The media the instrumented suite plays, all of it served by the hermetic
 * fixture server.
 */
object HermeticFixtures {

    private const val MP3_SIGNAL = "signal_mp3_track_sine440_187s.mp3"

    private const val KEY_HEX = "30313233343536373839616263646566"
    private const val IV_HEX = "00000000000000000000000000000000"

    private const val PACKAGED_AAC_LC =
        """"packaged_audio":{"codec":"aac_lc","sample_rate":44100,"channels":2,""" +
            """"timescale":44100,"bit_rate":128000,"gapless_encoding":"none",""" +
            """"source":{"Signal":"Sawtooth"}}"""

    private const val LADDER =
        """"variant_count":1,"segments_per_variant":3,"segment_duration_secs":4.0"""

    fun mp3(): String = TestServerFixture.signal(MP3_SIGNAL)

    fun hls(): String = TestServerFixture.createHls("{$PACKAGED_AAC_LC,$LADDER}")

    fun encryptedHls(): String = TestServerFixture.createHls(
        """{$PACKAGED_AAC_LC,"encryption":{"key_hex":"$KEY_HEX","iv_hex":"$IV_HEX"},$LADDER}""",
    )
}
