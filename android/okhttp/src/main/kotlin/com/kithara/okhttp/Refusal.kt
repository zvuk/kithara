package com.kithara.okhttp

import java.io.IOException
import java.net.UnknownServiceException
import java.security.cert.CertificateException
import javax.net.ssl.SSLHandshakeException
import javax.net.ssl.SSLPeerUnverifiedException

/**
 * Whether [error] is a refusal asking again cannot change, the failures
 * OkHttp itself does not retry on another route: the cleartext policy, an
 * unverified peer, or a handshake the trust configuration rejected.
 */
internal fun isPermanent(error: IOException): Boolean =
    error is UnknownServiceException ||
        error is SSLPeerUnverifiedException ||
        (error is SSLHandshakeException &&
            generateSequence(error.cause) { it.cause }.any { it is CertificateException })
