package com.telescope

import java.io.OutputStream
import java.net.Socket
import java.security.MessageDigest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

// Shared HTTP/1.1 subset: read request/body, check bearer token, write response
object HttpWire {
    const val MAX_HEADER_BYTES = 16 * 1024
    const val MAX_BODY_BYTES = 4 * 1024
    const val READ_TIMEOUT_MS = 5_000
    private const val BEARER_PREFIX = "Bearer "

    data class Request(
        val method: String,
        val path: String,
        val query: String,
        val headers: Map<String, String>,
        val leftoverBody: ByteArray,
    )

    // Reads and parses request line/headers, bounded by MAX_HEADER_BYTES; returns null on error. The buffered read can overshoot into the body, so leftoverBody carries those bytes forward for readBody() to prepend.
    fun readRequest(socket: Socket): Request? {
        val inp = socket.getInputStream()
        val buf = ByteArray(4096)
        val sb = StringBuilder()
        while (!sb.contains("\r\n\r\n") && !sb.contains("\n\n")) {
            if (sb.length >= MAX_HEADER_BYTES) {
                sendError(socket.getOutputStream(), 431, "Request Header Fields Too Large")
                socket.close()
                return null
            }
            val n = inp.read(buf)
            if (n <= 0) { socket.close(); return null }
            sb.append(String(buf, 0, n, Charsets.ISO_8859_1))
        }
        val raw = sb.toString()
        val term = if (raw.contains("\r\n\r\n")) "\r\n\r\n" else "\n\n"
        val termIdx = raw.indexOf(term)
        val headerPart = raw.substring(0, termIdx)
        val leftover = raw.substring(termIdx + term.length)

        val lines = headerPart.split("\r\n", "\n")
        val requestLine = lines.firstOrNull() ?: ""
        val parts = requestLine.split(" ")
        if (parts.size < 2 || (parts[0] != "GET" && parts[0] != "POST")) {
            sendError(socket.getOutputStream(), 400, "Bad Request")
            socket.close()
            return null
        }
        val method = parts[0]
        val fullPath = parts[1]
        val path = fullPath.substringBefore("?")
        val query = fullPath.substringAfter("?", "")

        val headers = mutableMapOf<String, String>()
        for (line in lines.drop(1)) {
            if (line.isBlank()) continue
            val idx = line.indexOf(':')
            if (idx <= 0) continue
            headers[line.substring(0, idx).trim().lowercase()] = line.substring(idx + 1).trim()
        }

        return Request(method, path, query, headers, leftover.toByteArray(Charsets.ISO_8859_1))
    }

    // Reads Content-Length bytes bounded by MAX_BODY_BYTES; null on error (response already sent)
    fun readBody(socket: Socket, request: Request): ByteArray? {
        val length = request.headers["content-length"]?.toIntOrNull()
        if (length == null || length < 0) {
            sendError(socket.getOutputStream(), 400, "Bad Request")
            return null
        }
        if (length > MAX_BODY_BYTES) {
            sendError(socket.getOutputStream(), 413, "Payload Too Large")
            return null
        }
        val out = ByteArray(length)
        val fromLeftover = minOf(request.leftoverBody.size, length)
        System.arraycopy(request.leftoverBody, 0, out, 0, fromLeftover)
        var read = fromLeftover
        val inp = socket.getInputStream()
        while (read < length) {
            val n = inp.read(out, read, length - read)
            if (n <= 0) { sendError(socket.getOutputStream(), 400, "Bad Request"); return null }
            read += n
        }
        return out
    }

    fun isJsonBody(request: Request): Boolean =
        request.headers["content-type"]?.startsWith("application/json") == true

    // Parses flat JSON object to string-keyed map; numeric 1 and string "1" both collapse to "1" since the camera-control parser downstream expects stringified values either way.
    fun parseJsonParams(body: ByteArray): Map<String, String>? {
        return try {
            Json.parseToJsonElement(String(body, Charsets.UTF_8))
                .jsonObject
                .mapValues { (_, v) -> v.jsonPrimitive.content }
        } catch (_: Exception) {
            null
        }
    }

    // Constant-time bearer-token check; null expected always fails closed
    fun bearerMatches(expected: String?, request: Request): Boolean {
        val want = expected ?: return false
        val header = request.headers["authorization"] ?: return false
        if (!header.startsWith(BEARER_PREFIX)) return false
        val provided = header.substring(BEARER_PREFIX.length)
        return MessageDigest.isEqual(
            want.toByteArray(Charsets.UTF_8),
            provided.toByteArray(Charsets.UTF_8),
        )
    }

    // Device-to-desktop only; no CORS header needed.
    fun sendJson(out: OutputStream, json: String) {
        val body = json.toByteArray(Charsets.UTF_8)
        val hdr = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n" +
            "Content-Length: ${body.size}\r\n\r\n"
        out.write(hdr.toByteArray(Charsets.UTF_8))
        out.write(body)
        out.flush()
    }

    fun sendError(out: OutputStream, code: Int, reason: String) {
        try {
            val body = Json.encodeToString(ApiError.serializer(), ApiError(reason)).toByteArray(Charsets.UTF_8)
            val hdr = "HTTP/1.1 $code $reason\r\nContent-Type: application/json\r\n" +
                "Content-Length: ${body.size}\r\nConnection: close\r\n\r\n"
            out.write(hdr.toByteArray(Charsets.UTF_8))
            out.write(body)
            out.flush()
        } catch (_: Exception) {}
    }
}
