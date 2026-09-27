package com.telescope

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.math.BigInteger
import java.net.InetAddress
import java.net.ServerSocket
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.MessageDigest
import java.security.PrivateKey
import java.security.SecureRandom
import java.security.Signature
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.spec.ECGenParameterSpec
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone
import javax.net.ServerSocketFactory
import javax.net.ssl.KeyManagerFactory
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLServerSocket

// This phone's TLS key and self-signed certificate; desktops pin the certificate's SHA-256 at pairing.
class TlsIdentity(val privateKey: PrivateKey, val certificate: X509Certificate) {

    // Lowercase hex SHA-256 of the certificate's DER: what the desktop stores and checks on every connection.
    val fingerprint: String = sha256Hex(certificate.encoded)

    fun serverSocketFactory(): ServerSocketFactory {
        val keyStore = KeyStore.getInstance("PKCS12").apply {
            load(null, null)
            setKeyEntry(ALIAS, privateKey, PASSWORD, arrayOf(certificate))
        }
        val kmf = KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm()).apply { init(keyStore, PASSWORD) }
        val context = SSLContext.getInstance("TLS").apply { init(kmf.keyManagers, null, SecureRandom()) }
        return ModernTlsServerSocketFactory(context.serverSocketFactory)
    }

    fun toPkcs12(): ByteArray {
        val keyStore = KeyStore.getInstance("PKCS12").apply {
            load(null, null)
            setKeyEntry(ALIAS, privateKey, PASSWORD, arrayOf(certificate))
        }
        return ByteArrayOutputStream().also { keyStore.store(it, PASSWORD) }.toByteArray()
    }

    companion object {
        private const val ALIAS = "telescope"
        // The file lives in app-private, no-backup storage; the sandbox protects it, not this password.
        private val PASSWORD = "telescope".toCharArray()

        fun generate(commonName: String = "Telescope phone"): TlsIdentity {
            val keyPair = KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }.generateKeyPair()
            return TlsIdentity(keyPair.private, SelfSignedCert.create(keyPair, commonName))
        }

        // Null when the bytes aren't an identity this app wrote.
        fun fromPkcs12(bytes: ByteArray): TlsIdentity? = try {
            val keyStore = KeyStore.getInstance("PKCS12").apply { load(ByteArrayInputStream(bytes), PASSWORD) }
            val key = keyStore.getKey(ALIAS, PASSWORD) as? PrivateKey
            val cert = keyStore.getCertificate(ALIAS) as? X509Certificate
            if (key != null && cert != null) TlsIdentity(key, cert) else null
        } catch (_: Exception) {
            null
        }

        fun sha256Hex(bytes: ByteArray): String =
            MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    }
}

// Hands out TLS server sockets limited to TLS 1.2 and 1.3.
private class ModernTlsServerSocketFactory(private val inner: ServerSocketFactory) : ServerSocketFactory() {
    private fun restrict(socket: ServerSocket): ServerSocket = socket.also {
        if (it is SSLServerSocket) {
            it.enabledProtocols = it.supportedProtocols.filter { p -> p == "TLSv1.3" || p == "TLSv1.2" }.toTypedArray()
        }
    }
    override fun createServerSocket(): ServerSocket = restrict(inner.createServerSocket())
    override fun createServerSocket(port: Int): ServerSocket = restrict(inner.createServerSocket(port))
    override fun createServerSocket(port: Int, backlog: Int): ServerSocket = restrict(inner.createServerSocket(port, backlog))
    override fun createServerSocket(port: Int, backlog: Int, address: InetAddress?): ServerSocket =
        restrict(inner.createServerSocket(port, backlog, address))
}

// Minimal DER writer for one self-signed X.509 v3 certificate (ECDSA P-256, no extensions); the JDK can't make one.
object SelfSignedCert {
    private val ECDSA_WITH_SHA256 = byteArrayOf(0x2a, 0x86.toByte(), 0x48, 0xce.toByte(), 0x3d, 0x04, 0x03, 0x02)
    private val COMMON_NAME = byteArrayOf(0x55, 0x04, 0x03)

    fun create(keyPair: KeyPair, commonName: String, notBefore: Date = Date(), years: Int = 100): X509Certificate {
        val notAfter = Date(notBefore.time + years * 365L * 24 * 60 * 60 * 1000)
        val algorithm = seq(tlv(0x06, ECDSA_WITH_SHA256))
        val name = seq(set(seq(tlv(0x06, COMMON_NAME), tlv(0x0c, commonName.toByteArray(Charsets.UTF_8)))))
        val serial = BigInteger(128, SecureRandom()).setBit(0)
        val tbs = seq(
            tlv(0xa0, tlv(0x02, byteArrayOf(2))),
            tlv(0x02, serial.toByteArray()),
            algorithm,
            name,
            seq(time(notBefore), time(notAfter)),
            name,
            keyPair.public.encoded,
        )
        val signature = Signature.getInstance("SHA256withECDSA").run {
            initSign(keyPair.private)
            update(tbs)
            sign()
        }
        val der = seq(tbs, algorithm, tlv(0x03, byteArrayOf(0) + signature))
        return CertificateFactory.getInstance("X.509").generateCertificate(ByteArrayInputStream(der)) as X509Certificate
    }

    // UTCTime through 2049, GeneralizedTime after, as RFC 5280 requires.
    private fun time(date: Date): ByteArray {
        val utc = TimeZone.getTimeZone("UTC")
        val year = java.util.Calendar.getInstance(utc).apply { time = date }.get(java.util.Calendar.YEAR)
        val (tag, pattern) = if (year < 2050) 0x17 to "yyMMddHHmmss'Z'" else 0x18 to "yyyyMMddHHmmss'Z'"
        val text = SimpleDateFormat(pattern, Locale.US).apply { timeZone = utc }.format(date)
        return tlv(tag, text.toByteArray(Charsets.US_ASCII))
    }

    private fun seq(vararg parts: ByteArray) = tlv(0x30, parts.fold(ByteArray(0)) { acc, p -> acc + p })
    private fun set(vararg parts: ByteArray) = tlv(0x31, parts.fold(ByteArray(0)) { acc, p -> acc + p })

    private fun tlv(tag: Int, content: ByteArray): ByteArray {
        val len = content.size
        val length = when {
            len < 0x80 -> byteArrayOf(len.toByte())
            len < 0x100 -> byteArrayOf(0x81.toByte(), len.toByte())
            else -> byteArrayOf(0x82.toByte(), (len shr 8).toByte(), len.toByte())
        }
        return byteArrayOf(tag.toByte()) + length + content
    }
}
