package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.net.ServerSocket
import java.net.Socket
import java.security.cert.X509Certificate
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

class TlsIdentityTest {

    @Test
    fun `a generated certificate is a valid self-signed P-256 certificate`() {
        val identity = TlsIdentity.generate("Test phone")
        val cert = identity.certificate
        cert.checkValidity()
        cert.verify(cert.publicKey)
        assertEquals(3, cert.version)
        assertEquals("SHA256withECDSA", cert.sigAlgName)
        assertTrue(cert.subjectX500Principal.name.contains("CN=Test phone"))
        assertTrue(cert.notAfter.time - cert.notBefore.time > 90L * 365 * 24 * 60 * 60 * 1000)
    }

    @Test
    fun `the fingerprint is the SHA-256 of the certificate and differs per identity`() {
        val a = TlsIdentity.generate()
        assertEquals(TlsIdentity.sha256Hex(a.certificate.encoded), a.fingerprint)
        assertEquals(64, a.fingerprint.length)
        assertNotEquals(a.fingerprint, TlsIdentity.generate().fingerprint)
    }

    @Test
    fun `an identity survives a round trip through its saved form`() {
        val identity = TlsIdentity.generate()
        val restored = TlsIdentity.fromPkcs12(identity.toPkcs12())
        assertEquals(identity.fingerprint, restored?.fingerprint)
        assertNull(TlsIdentity.fromPkcs12(byteArrayOf(1, 2, 3)))
    }

    @Test
    fun `the session server speaks TLS with the pinned certificate and ignores plain HTTP`() {
        val identity = TlsIdentity.generate()
        val commands = object : SessionCommands {
            override fun start() = ControlResult(ok = true)
            override fun stop() = ControlResult(ok = true)
            override fun snapshot() = SessionSnapshot(SessionServer.PROTOCOL_VERSION, false, false, false, "phone-1", "Test phone")
            override fun unpair(computer: PairedComputer) {}
        }
        val server = SessionServer(0, { PairedComputerList() }, commands, appVersion = "1", appBuild = 1,
            requestDeadlineMs = 1_000, socketFactory = identity.serverSocketFactory())
        server.start()
        try {
            val field = SessionServer::class.java.getDeclaredField("serverSocket").apply { isAccessible = true }
            val port = (field.get(server) as ServerSocket).localPort

            val trustAll = object : X509TrustManager {
                override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) {}
                override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {}
                override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
            }
            val context = SSLContext.getInstance("TLS").apply { init(null, arrayOf<TrustManager>(trustAll), null) }
            (context.socketFactory.createSocket("127.0.0.1", port) as SSLSocket).use { socket ->
                socket.soTimeout = 3_000
                socket.startHandshake()
                val peer = socket.session.peerCertificates[0]
                assertEquals(identity.fingerprint, TlsIdentity.sha256Hex(peer.encoded))
                assertTrue(socket.session.protocol == "TLSv1.3" || socket.session.protocol == "TLSv1.2")
                socket.outputStream.write("GET /v1/hello HTTP/1.1\r\n\r\n".toByteArray())
                val reply = socket.inputStream.readBytes().toString(Charsets.UTF_8)
                assertTrue(reply.startsWith("HTTP/1.1 200"))
                assertTrue(reply.contains("phone-1"))
            }

            Socket("127.0.0.1", port).use { socket ->
                socket.soTimeout = 3_000
                socket.getOutputStream().write("GET /v1/hello HTTP/1.1\r\n\r\n".toByteArray())
                val reply = runCatching { socket.getInputStream().readBytes().toString(Charsets.ISO_8859_1) }.getOrDefault("")
                assertTrue(!reply.contains("phone-1"))
            }
        } finally {
            server.stop()
        }
    }
}
