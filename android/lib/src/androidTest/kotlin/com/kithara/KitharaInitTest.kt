package com.kithara

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class KitharaInitTest {
    @Test
    fun multiplePlayersCanBeCreated() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        Kithara.initialize(context)

        val p1 = KitharaPlayer()
        val p2 = KitharaPlayer()
        assert(p1 !== p2)
    }
}
