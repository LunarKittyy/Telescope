package com.telescope

import java.io.OutputStream
import java.net.Socket
import java.security.MessageDigest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * The small HTTP/1.1 subset both of the app's servers speak, factored out of
 * [MjpegServer] so [SessionServer] gets the same bounded, already-exercised
 * reader instead of a second hand-rolled one. Nothing here knows about
 * cameras, streams or pairing - it reads a request, reads a body, checks a
 * bearer token, writes a response.
 *
 * Everything is deliberately blocking and socket-oriented: both callers run
 * one thread per connection with an `soTimeout` already set, so there is no
 * async machinery to justify.
 */
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

    /**
     * Reads and parses the request line and headers, bounded by [MAX_HEADER_BYTES]
     * so a client that never sends a terminator can't hold a thread's read buffer
     * open indefinitely. Only `GET`/`POST` are accepted. Returns null (having
     * already written an error response and closed the socket) on any parse
     * failure. Any body bytes already read past the header terminator while
     * filling the read buffer are preserved in [Request.leftoverBody] for
     * [readBody] to prepend.
     */
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

    /** Reads exactly `Content-Length` bytes of body, bounded by [MAX_BODY_BYTES].
     *  Returns null (having already written an error response) on any failure. */
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

    /** True when the request declares a JSON body. Both POST routes require it. */
    fun isJsonBody(request: Request): Boolean =
        request.headers["content-type"]?.startsWith("application/json") == true

    /**
     * Parses a flat JSON object body into a string-keyed param map - each
     * value's raw literal text (so a JSON number `1` and a JSON string `"1"`
     * both come out as the string "1", matching what the desktop's stringified
     * control payloads send and what the existing per-action
     * `toIntOrNull()`/`== "1"`-style parsing in
     * [CameraStreamService.handleControlCommand] already expects). Returns
     * null on any malformed or non-object input.
     */
    fun parseJsonParams(body: ByteArray): Map<String, String>? {
        return try {
            Json.parseToJsonElement(String(body, Charsets.UTF_8))
                .jsonObject
                .mapValues { (_, v) -> v.jsonPrimitive.content }
        } catch (_: Exception) {
            null
        }
    }

    /** Constant-time bearer-token check. A null [expected] (nothing paired yet)
     *  always fails closed. */
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

    // Device-to-desktop only; there is no browser-origin caller to grant CORS
    // access to, so no Access-Control-Allow-Origin header is sent.
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
