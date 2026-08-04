package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.net.ServerSocket
import java.net.Socket
import java.nio.charset.StandardCharsets

class SessionServerTest {

    private data class Response(val status: Int, val body: String)

    /** Records what the server asked it to do, and reports whatever state the
     *  test wants, so the routes can be exercised without a Service, a
     *  Context, or a camera. */
    private class FakeCommands(
        var snapshot: SessionSnapshot = SessionSnapshot(
            protocol = SessionServer.PROTOCOL_VERSION,
            streaming = false,
            busy = false,
            localOnly = false,
        ),
        var startResult: ControlResult = ControlResult(ok = true),
        var stopResult: ControlResult = ControlResult(ok = true),
    ) : SessionCommands {
        val calls = mutableListOf<String>()
        override fun start(): ControlResult { calls += "start"; return startResult }
        override fun stop(): ControlResult { calls += "stop"; return stopResult }
        override fun snapshot(): SessionSnapshot { calls += "snapshot"; return snapshot }
    }

    private fun actualPort(server: SessionServer): Int {
        val field = SessionServer::class.java.getDeclaredField("serverSocket")
        field.isAccessible = true
        return (field.get(server) as ServerSocket).localPort
    }

    private fun request(port: Int, raw: String): Response {
        Socket("127.0.0.1", port).use { socket ->
            socket.soTimeout = 2_000
            socket.getOutputStream().apply {
                write(raw.toByteArray(StandardCharsets.ISO_8859_1))
                flush()
            }
            val text = socket.getInputStream().readBytes().toString(StandardCharsets.UTF_8)
            val split = text.indexOf("\r\n\r\n")
            val headers = text.substring(0, split)
            val status = headers.lineSequence().first().split(" ")[1].toInt()
            return Response(status, text.substring(split + 4))
        }
    }

    private fun get(port: Int, path: String, token: String?): Response {
        val auth = if (token != null) "Authorization: Bearer $token\r\n" else ""
        return request(port, "GET $path HTTP/1.1\r\n$auth\r\n")
    }

    private fun post(port: Int, path: String, token: String?, body: String, contentType: String = "application/json"): Response {
        val bytes = body.toByteArray(StandardCharsets.UTF_8)
        val auth = if (token != null) "Authorization: Bearer $token\r\n" else ""
        return request(
            port,
            "POST $path HTTP/1.1\r\n$auth" +
                "Content-Type: $contentType\r\n" +
                "Content-Length: ${bytes.size}\r\n\r\n$body",
        )
    }

    private fun withServer(
        token: String? = "secret-token",
        commands: FakeCommands = FakeCommands(),
        block: (port: Int, commands: FakeCommands) -> Unit,
    ) {
        val server = SessionServer(0, { token }, commands)
        server.start()
        try {
            block(actualPort(server), commands)
        } finally {
            server.stop()
        }
    }

    // ── Routing (pure, no socket) ────────────────────────────────────────────

    @Test
    fun `route maps the two known paths and rejects everything else`() {
        assertEquals(SessionServer.Route.Ping, SessionServer.route("GET", "/v1/ping"))
        assertEquals(SessionServer.Route.Session, SessionServer.route("POST", "/v1/session"))
        assertEquals(SessionServer.Route.MethodNotAllowed, SessionServer.route("POST", "/v1/ping"))
        assertEquals(SessionServer.Route.MethodNotAllowed, SessionServer.route("GET", "/v1/session"))
        assertEquals(SessionServer.Route.NotFound, SessionServer.route("GET", "/v1/video"))
        assertEquals(SessionServer.Route.NotFound, SessionServer.route("GET", "/"))
    }

    // ── Auth ─────────────────────────────────────────────────────────────────

    @Test
    fun `ping reports the phone's state to a holder of the current token`() {
        val commands = FakeCommands(
            snapshot = SessionSnapshot(
                protocol = SessionServer.PROTOCOL_VERSION,
                streaming = true,
                busy = false,
                localOnly = true,
            ),
        )
        withServer(commands = commands) { port, _ ->
            val response = get(port, "/v1/ping", "secret-token")
            assertEquals(200, response.status)
            assertTrue(response.body.contains("\"streaming\":true"), response.body)
            assertTrue(response.body.contains("\"localOnly\":true"), response.body)
            assertTrue(
                response.body.contains("\"protocol\":${SessionServer.PROTOCOL_VERSION}"),
                response.body,
            )
        }
    }

    @Test
    fun `a mismatched or absent token gets 401 on both routes`() {
        withServer { port, commands ->
            assertEquals(401, get(port, "/v1/ping", "wrong-token").status)
            assertEquals(401, get(port, "/v1/ping", null).status)
            assertEquals(401, post(port, "/v1/session", "wrong-token", "{\"action\":\"start\"}").status)
            assertEquals(401, post(port, "/v1/session", null, "{\"action\":\"start\"}").status)
            // Nothing reached the camera lifecycle.
            assertFalse(commands.calls.contains("start"))
        }
    }

    @Test
    fun `an unpaired phone rejects every request rather than defaulting open`() {
        withServer(token = null) { port, _ ->
            assertEquals(401, get(port, "/v1/ping", "any-token").status)
            assertEquals(401, post(port, "/v1/session", "any-token", "{\"action\":\"stop\"}").status)
        }
    }

    @Test
    fun `the token is re-read per request so a re-pair takes effect immediately`() {
        var token: String? = "first-token"
        val server = SessionServer(0, { token }, FakeCommands())
        server.start()
        try {
            val port = actualPort(server)
            assertEquals(200, get(port, "/v1/ping", "first-token").status)
            token = "second-token"
            assertEquals(401, get(port, "/v1/ping", "first-token").status)
            assertEquals(200, get(port, "/v1/ping", "second-token").status)
        } finally {
            server.stop()
        }
    }

    // ── /v1/session ──────────────────────────────────────────────────────────

    @Test
    fun `start and stop reach the camera lifecycle and report its verdict`() {
        withServer { port, commands ->
            assertEquals(200, post(port, "/v1/session", "secret-token", "{\"action\":\"start\"}").status)
            assertEquals(200, post(port, "/v1/session", "secret-token", "{\"action\":\"stop\"}").status)
            assertEquals(listOf("start", "stop"), commands.calls)
        }
    }

    @Test
    fun `a refusal comes back as ok false with the reason intact`() {
        val commands = FakeCommands(
            startResult = ControlResult(ok = false, error = "no_camera_permission"),
        )
        withServer(commands = commands) { port, _ ->
            val response = post(port, "/v1/session", "secret-token", "{\"action\":\"start\"}")
            // Still HTTP 200: the request was well-formed and authorized, the
            // camera just wouldn't open. The desktop reads the body.
            assertEquals(200, response.status)
            assertTrue(response.body.contains("\"ok\":false"), response.body)
            assertTrue(response.body.contains("no_camera_permission"), response.body)
        }
    }

    @Test
    fun `an unknown action is refused without touching the camera`() {
        withServer { port, commands ->
            val response = post(port, "/v1/session", "secret-token", "{\"action\":\"selfdestruct\"}")
            assertEquals(200, response.status)
            assertTrue(response.body.contains("unknown action"), response.body)
            assertEquals(emptyList<String>(), commands.calls)
        }
    }

    @Test
    fun `a malformed or non-JSON body is a 400`() {
        withServer { port, commands ->
            assertEquals(400, post(port, "/v1/session", "secret-token", "not json at all").status)
            assertEquals(
                400,
                post(port, "/v1/session", "secret-token", "{\"action\":\"start\"}", contentType = "text/plain").status,
            )
            assertEquals(emptyList<String>(), commands.calls)
        }
    }

    // ── Method/path handling ─────────────────────────────────────────────────

    @Test
    fun `unknown paths 404 and known paths reject the wrong method`() {
        withServer { port, _ ->
            assertEquals(404, get(port, "/v1/video", "secret-token").status)
            assertEquals(405, get(port, "/v1/session", "secret-token").status)
            assertEquals(405, post(port, "/v1/ping", "secret-token", "{}").status)
        }
    }

    @Test
    fun `the wrong method is rejected before the token is even consulted`() {
        // A path/method mismatch is not an authorization question, and
        // answering 405 without checking the token keeps the two concerns
        // from having to agree about ordering.
        withServer { port, _ ->
            assertEquals(405, get(port, "/v1/session", "wrong-token").status)
        }
    }
}
