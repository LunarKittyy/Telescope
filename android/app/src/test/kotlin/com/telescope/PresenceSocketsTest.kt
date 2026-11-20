package com.telescope

import java.net.InetSocketAddress
import java.nio.ByteBuffer
import java.nio.channels.DatagramChannel
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PresenceSocketsTest {
    private fun loopback(port: Int = 0) = InetSocketAddress("127.0.0.1", port)

    @Test
    fun `restart rebinds before the previous worker finishes`() {
        val old = PresenceSockets()
        val executor = Executors.newSingleThreadExecutor()
        val bound = CountDownLatch(1)
        val finish = CountDownLatch(1)
        var channel: DatagramChannel? = null
        val worker = executor.submit {
            try {
                channel = old.open { it.bind(loopback()) }
                bound.countDown()
                assertTrue(finish.await(2, TimeUnit.SECONDS))
            } finally {
                old.close()
            }
        }
        try {
            assertTrue(bound.await(2, TimeUnit.SECONDS))
            val address = channel!!.localAddress
            old.close()
            assertFalse(channel!!.isOpen)
            assertFalse(worker.isDone)
            PresenceSockets().use { replacement ->
                val rebound = replacement.open { it.bind(address) }!!
                DatagramChannel.open().use { sender ->
                    sender.send(ByteBuffer.wrap(byteArrayOf(1)), address)
                    assertEquals(listOf(rebound), replacement.readable(1_000))
                }
            }
        } finally {
            finish.countDown()
            old.close()
            try {
                worker.get(2, TimeUnit.SECONDS)
            } finally {
                executor.shutdownNow()
            }
        }
    }

    @Test
    fun `stop closes an in flight bind and prevents later binds`() {
        val sockets = PresenceSockets()
        val executor = Executors.newFixedThreadPool(2)
        val binding = CountDownLatch(1)
        val finishBind = CountDownLatch(1)
        val stoppingStarted = CountDownLatch(1)
        val opening = executor.submit<DatagramChannel?> {
            sockets.open {
                it.bind(loopback())
                binding.countDown()
                assertTrue(finishBind.await(2, TimeUnit.SECONDS))
            }
        }
        try {
            assertTrue(binding.await(2, TimeUnit.SECONDS))
            val stopping = executor.submit { stoppingStarted.countDown(); sockets.close() }
            assertTrue(stoppingStarted.await(2, TimeUnit.SECONDS))
            finishBind.countDown()
            stopping.get(2, TimeUnit.SECONDS)
            val channel = opening.get(2, TimeUnit.SECONDS)!!
            assertFalse(channel.isOpen)
            assertNull(sockets.open { error("A stopped worker must not bind") })
        } finally {
            finishBind.countDown()
            sockets.close()
            executor.shutdownNow()
        }
    }

    @Test
    fun `hints wake an indefinite wait even without sockets`() {
        PresenceSockets().use { sockets ->
            val executor = Executors.newSingleThreadExecutor()
            val started = CountDownLatch(1)
            try {
                val waiting = executor.submit<List<DatagramChannel>> {
                    started.countDown()
                    sockets.readable(null)
                }
                assertTrue(started.await(2, TimeUnit.SECONDS))
                sockets.wakeup()
                assertTrue(waiting.get(2, TimeUnit.SECONDS).isEmpty())
                sockets.wakeup()
                assertTrue(sockets.readable(60_000).isEmpty())
            } finally {
                sockets.wakeup()
                executor.shutdownNow()
            }
        }
    }

    @Test
    fun `stop wakes a worker blocked in the selector`() {
        val sockets = PresenceSockets()
        val executor = Executors.newSingleThreadExecutor()
        val started = CountDownLatch(1)
        try {
            val waiting = executor.submit {
                started.countDown()
                runCatching { sockets.readable(null) }
            }
            assertTrue(started.await(2, TimeUnit.SECONDS))
            sockets.close()
            waiting.get(2, TimeUnit.SECONDS)
        } finally {
            sockets.close()
            executor.shutdownNow()
        }
    }

    @Test
    fun `timer expires without a datagram and zero timeout never blocks`() {
        PresenceSockets().use { sockets ->
            val start = System.nanoTime()
            assertTrue(sockets.readable(30).isEmpty())
            assertTrue(System.nanoTime() - start >= TimeUnit.MILLISECONDS.toNanos(20))
            assertTrue(sockets.readable(0).isEmpty())
        }
    }
}
