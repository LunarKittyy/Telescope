package com.telescope

import android.content.Context
import android.os.Build
import android.provider.Settings
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import java.security.MessageDigest
import java.util.UUID

// One desktop this phone has paired with. Each gets its own bearer token, so removing one leaves the others working.
@Serializable
data class PairedComputer(
    val id: String,
    val name: String,
    val token: String,
    val pairedAtMs: Long,
    val addresses: List<PairingCandidate> = emptyList(),
    val desktopVersion: String? = null,
    val presence: Boolean? = null,
)

// The paired-computer list as a value; Android-free so it's unit-testable on the JVM.
class PairedComputerList(val computers: List<PairedComputer> = emptyList()) {

    // Re-pairing the same computer replaces its entry (and so revokes its old token) rather than adding a second one.
    fun withAdded(computer: PairedComputer): PairedComputerList =
        PairedComputerList(computers.filter { it.id != computer.id && it.token != computer.token } + computer)

    fun without(id: String): PairedComputerList = PairedComputerList(computers.filter { it.id != id })

    // The order the Manage screen lists them in
    val newestFirst: List<PairedComputer> get() = computers.sortedByDescending { it.pairedAtMs }

    // Remove all only once there are two to remove
    val offersRemoveAll: Boolean get() = computers.size >= 2

    // Constant-time per comparison, and every entry is compared so timing doesn't reveal which one matched.
    fun matchToken(provided: String?): PairedComputer? {
        if (provided.isNullOrEmpty()) return null
        val given = provided.toByteArray(Charsets.UTF_8)
        var match: PairedComputer? = null
        for (c in computers) {
            if (MessageDigest.isEqual(c.token.toByteArray(Charsets.UTF_8), given)) match = c
        }
        return match
    }

    fun encode(): String = Json.encodeToString(ListSerializer(PairedComputer.serializer()), computers)

    companion object {
        private val json = Json { ignoreUnknownKeys = true }

        // Malformed storage means no pairings rather than a crash; the user just pairs again.
        fun decode(raw: String?): PairedComputerList = try {
            if (raw.isNullOrBlank()) PairedComputerList()
            else PairedComputerList(json.decodeFromString(ListSerializer(PairedComputer.serializer()), raw))
        } catch (_: Exception) {
            PairedComputerList()
        }
    }
}

// Persisted pairings plus this phone's stable identity. Both live in the "telescope_pairing" prefs file,
// which backup_rules.xml / data_extraction_rules.xml keep out of backups and device transfer: the tokens
// are credentials, and a restored phone should get a fresh id rather than impersonate the old one.
object PairedComputers {
    private const val PREFS = "telescope_pairing"
    private const val KEY_COMPUTERS = "computers"
    private const val KEY_PHONE_ID = "phone_id"
    // LEGACY MIGRATION PATH: remove with the line in list() once the next stable release has been out a while.
    // Single-token storage from pairing protocol v2. Dropped on first read: v3 pairings carry a
    // computer id/name, so the old token can't be carried over and the phone is paired again once.
    private const val KEY_LEGACY_TOKEN = "active_token"

    @Volatile private var cache: PairedComputerList? = null
    private val listeners = mutableSetOf<() -> Unit>()
    // Computers that answered since this app process started; the stored version is only trusted for these
    private val heardThisRun = mutableSetOf<String>()

    @Synchronized fun heardThisRun(id: String): Boolean = id in heardThisRun

    @Synchronized
    fun list(context: Context): PairedComputerList {
        cache?.let { return it }
        val prefs = prefs(context)
        // LEGACY MIGRATION PATH
        if (prefs.contains(KEY_LEGACY_TOKEN)) prefs.edit().remove(KEY_LEGACY_TOKEN).apply()
        return PairedComputerList.decode(prefs.getString(KEY_COMPUTERS, null)).also { cache = it }
    }

    fun tokens(context: Context): List<String> = list(context).computers.map { it.token }

    fun add(context: Context, computer: PairedComputer) = update(context) { it.withAdded(computer) }

    fun remove(context: Context, id: String) = update(context) { it.without(id) }

    fun removeAll(context: Context) = update(context) { PairedComputerList() }

    fun authenticatedContact(context: Context, computer: PairedComputer, headers: Map<String, String>, address: String?) {
        val addresses = headers["x-telescope-addresses"].orEmpty().split(',') + listOfNotNull(address)
        contact(context, computer, headers["x-telescope-version"]?.take(64) ?: "", headers["x-telescope-presence"] == "1", addresses)
    }

    internal fun presenceContact(context: Context, update: PresenceContact) =
        contact(context, update.computer, update.version, true, listOf(update.address) + update.addresses)

    private fun contact(context: Context, computer: PairedComputer, version: String, capable: Boolean, addresses: List<String>) {
        val first = synchronized(this) { heardThisRun.add(computer.id) }
        val before = list(context)
        updateContact(context, computer, version, capable, addresses)
        if (first && list(context).computers == before.computers) synchronized(this) { listeners.toList() }.forEach { it() }
    }

    private fun updateContact(context: Context, computer: PairedComputer, version: String, capable: Boolean, addresses: List<String>) = update(context) { list ->
        PairedComputerList(list.computers.map { current ->
            if (current.id != computer.id || current.token != computer.token) current
            else current.copy(desktopVersion = version, presence = capable, addresses = mergeComputerAddresses(addresses, current.addresses))
        })
    }

    @Synchronized
    private fun update(context: Context, change: (PairedComputerList) -> PairedComputerList) {
        val next = change(list(context))
        if (next.computers == list(context).computers) return
        prefs(context).edit().putString(KEY_COMPUTERS, next.encode()).apply()
        cache = next
        listeners.toList().forEach { it() }
    }

    // MainActivity redraws its list when a pairing arrives or is revoked from the desktop.
    @Synchronized fun addListener(listener: () -> Unit) { listeners += listener }
    @Synchronized fun removeListener(listener: () -> Unit) { listeners -= listener }

    @Synchronized
    fun phoneId(context: Context): String {
        val prefs = prefs(context)
        prefs.getString(KEY_PHONE_ID, null)?.let { return it }
        val id = UUID.randomUUID().toString()
        prefs.edit().putString(KEY_PHONE_ID, id).apply()
        return id
    }

    // The name the user gave the phone in Android settings ("Pixel 8 Pro"), else the model.
    fun phoneName(context: Context): String =
        Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
            ?.takeIf { it.isNotBlank() } ?: Build.MODEL

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}

// Existing adapter kinds remain useful when HTTPS only supplies literal addresses.
internal fun mergeComputerAddresses(incoming: List<String>, previous: List<PairingCandidate>): List<PairingCandidate> =
    boundedPresenceAddresses(incoming + previous.map { it.ip }).map { ip ->
        previous.firstOrNull { it.ip == ip } ?: PairingCandidate(ip, "", presenceKind(ip))
    }
