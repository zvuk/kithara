package com.kithara.ffi

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class FfiFileSourceSettingsTest {
    @Test
    fun readerEventConstructorRemainsAvailable() {
        val legacy = FfiFileSourceSettings(readerEventCapacity = 512u)
        val bounded = FfiFileSourceSettings(lookAheadBytes = 0uL, readerEventCapacity = 512u)

        assertEquals(512u, legacy.readerEventCapacity)
        assertNull(legacy.lookAheadBytes)
        assertEquals(0uL, bounded.lookAheadBytes)
    }
}
