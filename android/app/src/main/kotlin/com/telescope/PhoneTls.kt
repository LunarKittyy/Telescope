package com.telescope

import android.content.Context
import java.io.File

// This install's TLS identity, made on first use and kept in no-backup storage: a restored phone gets a new one and pairs again.
object PhoneTls {
    private const val FILE = "tls_identity.p12"

    @Volatile private var cached: TlsIdentity? = null

    @Synchronized
    fun identity(context: Context): TlsIdentity {
        cached?.let { return it }
        val file = File(context.applicationContext.noBackupFilesDir, FILE)
        val loaded = if (file.exists()) TlsIdentity.fromPkcs12(file.readBytes()) else null
        val identity = loaded ?: TlsIdentity.generate().also { save(file, it) }
        cached = identity
        return identity
    }

    private fun save(file: File, identity: TlsIdentity) {
        val partial = File(file.path + ".part")
        partial.writeBytes(identity.toPkcs12())
        if (!partial.renameTo(file)) {
            partial.delete()
            throw java.io.IOException("Couldn't save the TLS identity")
        }
    }
}
