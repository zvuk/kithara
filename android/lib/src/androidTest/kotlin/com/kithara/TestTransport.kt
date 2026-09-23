package com.kithara

import com.kithara.net.HttpTransport
import com.kithara.okhttp.OkHttpTransport
import okhttp3.OkHttpClient

object TestTransport {
    val okHttp: HttpTransport by lazy { OkHttpTransport(OkHttpClient()) }
}
