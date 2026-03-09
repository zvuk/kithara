package com.kithara

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class KitharaInitTest {
    @Test
    fun initializeIsIdempotent() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val config = KitharaConfig(
            store = KitharaStoreOptions(cacheDir = context.cacheDir.absolutePath),
        )

        Kithara.initialize(context, config)
        Kithara.initialize(context, config)
    }
}
