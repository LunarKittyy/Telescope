package com.telescope

import java.nio.channels.DatagramChannel
import java.nio.channels.SelectionKey
import java.nio.channels.Selector

internal class PresenceSockets : AutoCloseable {
    private val selector = Selector.open()
    private val channels = mutableListOf<DatagramChannel>()
    private var closed = false

    @Synchronized
    fun open(bind: (DatagramChannel) -> Unit): DatagramChannel? {
        if (closed) return null
        val channel = DatagramChannel.open()
        try {
            bind(channel)
            channel.configureBlocking(false)
            channel.register(selector, SelectionKey.OP_READ)
            channels.removeAll { !it.isOpen }
            channels += channel
            return channel
        } catch (e: Exception) {
            channel.close()
            throw e
        }
    }

    fun wakeup() { selector.wakeup() }

    fun readable(timeout: Long?): List<DatagramChannel> {
        when {
            timeout == null -> selector.select()
            timeout <= 0 -> selector.selectNow()
            else -> selector.select(timeout)
        }
        val ready = selector.selectedKeys().filter { it.isValid && it.isReadable }.map { it.channel() as DatagramChannel }
        selector.selectedKeys().clear()
        return ready
    }

    @Synchronized
    override fun close() {
        if (closed) return
        closed = true
        selector.wakeup()
        channels.forEach { runCatching { it.close() } }
        channels.clear()
        selector.close()
    }
}
