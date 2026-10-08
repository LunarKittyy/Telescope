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

    // Test double: records invocations, returns injected state; exercises routes without Service/Context/camera.
    private class FakeCommands(
        var snapshot: SessionSnapshot = SessionSnapshot(
            protocol = SessionServer.PROTOCOL_VERSION,
            streaming = false,
            busy = false,
            localOnly = false,
            phoneId = "phone-1",
            phoneName = "Test phone",
        ),
        var startResult: ControlResult = ControlResult(ok = true),
        var stopResult: ControlResult = ControlResult(ok = true),
    ) : SessionCommands {
        val calls = mutableListOf<String>()
        val openings = mutableListOf<StreamOpening?>()
        override fun start(opening: StreamOpening?): ControlResult { calls += "start"; openings += opening; return startResult }
        override fun stop(): ControlResult { calls += "stop"; return stopResult }
        override fun snapshot(): SessionSnapshot { calls += "snapshot"; return snapshot }
        val unpaired = mutableListOf<String>()
        override fun unpair(computer: PairedComputer) { unpaired += computer.id }
    }

    private fun computersOf(vararg tokens: String?) = PairedComputerList(
        tokens.filterNotNull().mapIndexed { i, t -> PairedComputer("pc-$i", "PC $i", t, 0L) }
    )

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
        requestDeadlineMs: Int = HttpWire.REQUEST_DEADLINE_MS,
        localOnly: Boolean = false,
        overUsb: Boolean = true,
        block: (port: Int, commands: FakeCommands) -> Unit,
    ) {
        val server = SessionServer(0, { computersOf(token) }, commands, appVersion = "1.2.3", appBuild = 42,
            requestDeadlineMs = requestDeadlineMs, localOnly = { localOnly }, isUsbPeer = { overUsb })
        server.start()
        try {
            block(actualPort(server), commands)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `route maps the two known paths and rejects everything else`() {
        assertEquals(SessionServer.Route.Ping, SessionServer.route("GET", "/v1/ping"))
        assertEquals(SessionServer.Route.Session, SessionServer.route("POST", "/v1/session"))
        assertEquals(SessionServer.Route.MethodNotAllowed, SessionServer.route("POST", "/v1/ping"))
        assertEquals(SessionServer.Route.MethodNotAllowed, SessionServer.route("GET", "/v1/session"))
        assertEquals(SessionServer.Route.NotFound, SessionServer.route("GET", "/v1/video"))
        assertEquals(SessionServer.Route.NotFound, SessionServer.route("GET", "/"))
    }

    @Test
    fun `ping reports the phone's state to a holder of the current token`() {
        val commands = FakeCommands(
            snapshot = SessionSnapshot(
                protocol = SessionServer.PROTOCOL_VERSION,
                streaming = true,
                busy = false,
                localOnly = true,
                phoneId = "phone-1",
                phoneName = "Test phone",
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
        val server = SessionServer(0, { computersOf(token) }, FakeCommands())
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

    @Test
    fun `start and stop reach the camera lifecycle and report its verdict`() {
        withServer { port, commands ->
            assertEquals(200, post(port, "/v1/session", "secret-token", "{\"action\":\"start\"}").status)
            assertEquals(200, post(port, "/v1/session", "secret-token", "{\"action\":\"stop\"}").status)
            assertEquals(listOf("start", "stop"), commands.calls)
        }
    }

    @Test
    fun `a start can say what size and rate to open at`() {
        withServer { port, commands ->
            val body = "{\"action\":\"start\",\"width\":1920,\"height\":1080,\"fps\":60}"
            assertEquals(200, post(port, "/v1/session", "secret-token", body).status)
            assertEquals(listOf(StreamOpening(1920, 1080, 60)), commands.openings)
        }
    }

    @Test
    fun `a refusal comes back as ok false with the reason intact`() {
        val commands = FakeCommands(
            startResult = ControlResult(ok = false, error = "no_camera_permission"),
        )
        withServer(commands = commands) { port, _ ->
            val response = post(port, "/v1/session", "secret-token", "{\"action\":\"start\"}")
            // HTTP 200: request well-formed; camera startup failed (desktop reads body).
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

    @Test
    fun `a deeply nested body is a 400 and the server keeps answering`() {
        withServer { port, commands ->
            assertEquals(400, post(port, "/v1/session", "secret-token", "[".repeat(4000)).status)
            assertEquals(400, post(port, "/v1/session", "secret-token", "{\"action\":" + "[".repeat(4000)).status)
            assertEquals(200, get(port, "/v1/ping", "secret-token").status)
            assertEquals(emptyList<String>(), commands.calls.filter { it != "snapshot" })
        }
    }

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
        // Check method before token; keeps concerns independent.
        withServer { port, _ ->
            assertEquals(405, get(port, "/v1/session", "wrong-token").status)
        }
    }

    @Test
    fun `every paired computer's token is accepted`() {
        val server = SessionServer(0, { computersOf("desk-token", "laptop-token") }, FakeCommands())
        server.start()
        try {
            val port = actualPort(server)
            assertEquals(200, get(port, "/v1/ping", "desk-token").status)
            assertEquals(200, get(port, "/v1/ping", "laptop-token").status)
            assertEquals(401, get(port, "/v1/ping", "someone-else").status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `unpair revokes only the computer that asks`() {
        val commands = FakeCommands()
        val server = SessionServer(0, { computersOf("desk-token", "laptop-token") }, commands)
        server.start()
        try {
            val port = actualPort(server)
            assertEquals(200, post(port, "/v1/unpair", "laptop-token", "{}").status)
            assertEquals(listOf("pc-1"), commands.unpaired)
            assertEquals(401, post(port, "/v1/unpair", "unknown", "{}").status)
            assertEquals(listOf("pc-1"), commands.unpaired)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `ping tells the desktop which phone answered`() {
        withServer { port, _ ->
            val body = get(port, "/v1/ping", "secret-token").body
            assertTrue(body.contains("\"phoneId\":\"phone-1\""), body)
            assertTrue(body.contains("\"phoneName\":\"Test phone\""), body)
        }
    }

    @Test
    fun `hello names the phone without a token and reveals nothing else`() {
        withServer { port, commands ->
            val response = get(port, "/v1/hello", null)
            assertEquals(200, response.status)
            assertTrue(response.body.contains("\"phoneId\":\"phone-1\""), response.body)
            assertTrue(response.body.contains("\"appVersion\":\"1.2.3\""), response.body)
            assertTrue(response.body.contains("\"build\":42"), response.body)
            assertFalse(response.body.contains("streaming"), response.body)
            assertFalse(commands.calls.contains("start"))
        }
    }

    @Test
    fun `in Local only a computer off USB can only ping and unpair`() = withServer(localOnly = true, overUsb = false) { port, commands ->
        assertEquals(403, post(port, "/v1/session", "secret-token", "{\"action\":\"start\"}").status)
        assertEquals(403, get(port, "/v1/hello", null).status)
        assertEquals(200, get(port, "/v1/ping", "secret-token").status)
        assertEquals(401, get(port, "/v1/ping", "wrong").status)
        assertEquals(200, post(port, "/v1/unpair", "secret-token", "{}").status)
        assertFalse(commands.calls.contains("start"))
    }

    @Test
    fun `in Local only USB still gets everything`() = withServer(localOnly = true, overUsb = true) { port, commands ->
        assertEquals(200, get(port, "/v1/hello", null).status)
        assertEquals(200, post(port, "/v1/session", "secret-token", "{\"action\":\"start\"}").status)
        assertTrue(commands.calls.contains("start"))
    }

    @Test
    fun `with Local only off Wi-Fi gets everything`() = withServer(localOnly = false, overUsb = false) { port, commands ->
        assertEquals(200, post(port, "/v1/session", "secret-token", "{\"action\":\"start\"}").status)
        assertEquals(listOf("start"), commands.calls)
    }

    @Test
    fun `a peer trickling its request a byte at a time is cut off at the deadline`() = withServer(requestDeadlineMs = 300) { port, commands ->
        Socket("127.0.0.1", port).use { socket ->
            socket.soTimeout = 3_000
            val out = socket.getOutputStream()
            val started = System.currentTimeMillis()
            val closed = runCatching {
                for (b in "GET /v1/hello HTTP/1.1\r\nX-Slow: ".toByteArray()) {
                    out.write(b.toInt()); out.flush()
                    Thread.sleep(50)
                }
                repeat(40) { out.write('a'.code); out.flush(); Thread.sleep(50) }
            }.isFailure || socket.getInputStream().read() == -1
            assertTrue(closed)
            assertTrue(System.currentTimeMillis() - started < 2_500)
        }
        assertTrue(commands.calls.isEmpty())
    }

    @Test
    fun `idle connections from one address don't lock out another`() = withServer(requestDeadlineMs = 5_000) { port, _ ->
        val idle = (1..PendingLimiter.MAX_PER_ADDRESS + 2).map {
            Socket().apply { bind(java.net.InetSocketAddress("127.0.0.2", 0)); connect(java.net.InetSocketAddress("127.0.0.1", port)) }
        }
        try {
            assertEquals(200, get(port, "/v1/hello", null).status)
        } finally {
            idle.forEach { it.close() }
        }
    }
}
