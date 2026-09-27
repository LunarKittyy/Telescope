package com.telescope

// Caps connections still sending their request, per sender and in total, so one LAN device can't hold every slot.
class PendingLimiter(private val maxTotal: Int = MAX_TOTAL, private val maxPerAddress: Int = MAX_PER_ADDRESS) {
    private val perAddress = mutableMapOf<String, Int>()
    private var total = 0

    @Synchronized
    fun tryAcquire(address: String): Boolean {
        val held = perAddress[address] ?: 0
        if (total >= maxTotal || held >= maxPerAddress) return false
        perAddress[address] = held + 1
        total++
        return true
    }

    @Synchronized
    fun release(address: String) {
        val held = perAddress[address] ?: return
        if (held <= 1) perAddress.remove(address) else perAddress[address] = held - 1
        total--
    }

    companion object {
        const val MAX_TOTAL = 32
        const val MAX_PER_ADDRESS = 4
    }
}
