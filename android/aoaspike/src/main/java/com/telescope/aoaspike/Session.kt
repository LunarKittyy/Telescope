package com.telescope.aoaspike

import android.util.Log
import java.io.InputStream
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.ConcurrentHashMap
import kotlin.concurrent.thread

// One bench link: frames are 0xA5, type, stream u16, len u32 (LE), payload, at most 16384 bytes
class Session(private val input: InputStream, private val output: OutputStream) {
    private companion object {
        const val BATCH = 16384
        const val HDR = 8
        const val MAX_PAYLOAD = BATCH - HDR
    }

    private val streams = ConcurrentHashMap<Int, Socket>()
    private val writeLock = Object()
    private val out = ByteArray(BATCH)
    private var sinkBytes = 0L

    private fun send(type: Int, stream: Int, p: ByteArray, off: Int, len: Int) {
        synchronized(writeLock) {
            val b = ByteBuffer.wrap(out).order(ByteOrder.LITTLE_ENDIAN)
            b.put(0xA5.toByte()).put(type.toByte()).putShort(stream.toShort()).putInt(len)
            b.put(p, off, len)
            output.write(out, 0, HDR + len)
        }
    }

    private fun sendLong(type: Int, v: Long) {
        val b = ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN).putLong(v).array()
        send(type, 0, b, 0, 8)
    }

    // Accessory reads must always ask for the full 16 KB, so bytes are collected here rather than read to size
    fun run() {
        val tmp = ByteArray(BATCH)
        var acc = ByteArray(BATCH * 4)
        var have = 0
        Log.i(LinkTag, "session start")
        while (true) {
            val n = input.read(tmp)
            if (n < 0) {
                Log.i(LinkTag, "session eof")
                return
            }
            System.arraycopy(tmp, 0, acc, have, n)
            have += n
            var pos = 0
            while (have - pos >= HDR) {
                val h = ByteBuffer.wrap(acc, pos, HDR).order(ByteOrder.LITTLE_ENDIAN)
                val magic = h.get().toInt() and 0xFF
                val type = h.get().toInt() and 0xFF
                val stream = h.getShort().toInt() and 0xFFFF
                val len = h.getInt()
                if (magic != 0xA5 || len > MAX_PAYLOAD) throw IllegalStateException("desync $magic $len")
                if (have - pos - HDR < len) break
                frame(type, stream, acc, pos + HDR, len)
                pos += HDR + len
            }
            System.arraycopy(acc, pos, acc, 0, have - pos)
            have -= pos
        }
    }

    private fun frame(type: Int, stream: Int, b: ByteArray, off: Int, len: Int) {
        val p = ByteBuffer.wrap(b, off, len).order(ByteOrder.LITTLE_ENDIAN)
        when (type) {
            1 -> send(2, stream, b, off, len)
            3 -> sinkBytes += len
            4 -> source(p.getInt(), p.getInt(), p.getInt())
            7 -> {
                sendLong(8, sinkBytes)
                sinkBytes = 0
            }
            9 -> open(stream, p.getShort().toInt() and 0xFFFF)
            10 -> streams[stream]?.let {
                try {
                    it.getOutputStream().write(b, off, len)
                } catch (e: Exception) {
                    close(stream)
                }
            }
            11 -> streams.remove(stream)?.close()
            12 -> {
                // stream 0 asks the phone to write a frame of that size; stream 1 is the frame itself arriving
                val size = if (stream == 0) p.getInt() else 0
                send(13, 0, ByteArray(size), 0, size)
            }
            14 -> {
                sinkBytes = 0
                Log.i(LinkTag, "session reset by host")
            }
        }
    }

    private fun close(stream: Int) {
        streams.remove(stream)?.close()
        send(11, stream, ByteArray(0), 0, 0)
    }

    private fun open(stream: Int, port: Int) {
        // Connects inline so data following OPEN finds the socket; loopback connects take no time
        val s = Socket()
        try {
            s.tcpNoDelay = true
            s.connect(InetSocketAddress("127.0.0.1", port), 3000)
            streams[stream] = s
        } catch (e: Exception) {
            Log.i(LinkTag, "tunnel $stream to $port: $e")
            close(stream)
            return
        }
        thread(name = "tunnel-$stream") {
            try {
                val buf = ByteArray(MAX_PAYLOAD)
                val i = s.getInputStream()
                while (true) {
                    val n = i.read(buf)
                    if (n < 0) break
                    send(10, stream, buf, 0, n)
                }
            } catch (e: Exception) {
                Log.i(LinkTag, "tunnel $stream to $port: $e")
            }
            close(stream)
        }
    }

    private fun source(burst: Int, intervalMs: Int, secs: Int) {
        thread(name = "source") {
            val data = ByteArray(MAX_PAYLOAD)
            var sent = 0L
            val t0 = System.nanoTime()
            val end = t0 + secs * 1_000_000_000L
            var tick = t0
            try {
                while (System.nanoTime() < end) {
                    var left = burst
                    while (left > 0) {
                        val n = minOf(left, MAX_PAYLOAD)
                        send(5, 0, data, 0, n)
                        left -= n
                        sent += n
                    }
                    if (intervalMs > 0) {
                        tick += intervalMs * 1_000_000L
                        val wait = (tick - System.nanoTime()) / 1_000_000L
                        if (wait > 0) Thread.sleep(wait)
                    }
                }
                sendLong(6, sent)
            } catch (e: Exception) {
                Log.i(LinkTag, "source ended: $e")
            }
        }
    }
}

private const val LinkTag = "AoaSpike"
