package com.kithara.okhttp

import java.io.IOException
import java.net.UnknownServiceException
import java.security.cert.CertificateException
import javax.net.ssl.SSLHandshakeException
import javax.net.ssl.SSLPeerUnverifiedException
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RefusalTest {

    @Test
    fun theCleartextPolicyIsPermanent() {
        assertTrue(isPermanent(UnknownServiceException("CLEARTEXT communication not permitted")))
    }

    @Test
    fun anUnverifiedPeerIsPermanent() {
        assertTrue(isPermanent(SSLPeerUnverifiedException("Hostname not verified")))
    }

    @Test
    fun aHandshakeTheTrustConfigurationRejectedIsPermanent() {
        val handshake = SSLHandshakeException("handshake failed")
        handshake.initCause(RuntimeException(CertificateException("Trust anchor not found")))

        assertTrue(isPermanent(handshake))
    }

    @Test
    fun aHandshakeWithoutACertificateCauseIsTransient() {
        val handshake = SSLHandshakeException("Connection reset by peer")
        handshake.initCause(IOException("Connection reset by peer"))

        assertFalse(isPermanent(handshake))
    }

    @Test
    fun anIoFailureIsTransient() {
        assertFalse(isPermanent(IOException("unexpected end of stream")))
    }
}
