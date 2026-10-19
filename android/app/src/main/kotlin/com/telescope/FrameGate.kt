package com.telescope

import android.graphics.SurfaceTexture
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLExt
import android.opengl.EGLSurface
import android.opengl.GLES11Ext
import android.opengl.GLES20
import android.os.Handler
import android.os.HandlerThread
import android.view.Surface
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

// Between the camera and an encoder that ignores KEY_MAX_FPS_TO_ENCODER: passes only maxFps frames a second, so a step needn't restart the camera
class FrameGate(width: Int, height: Int, output: Surface, private val onError: (Throwable) -> Unit) {

    /** The most frames a second to pass on, or null for all of them. */
    @Volatile var maxFps: Int? = null

    val inputSurface: Surface

    private val thread = HandlerThread("frame-gate").also { it.start() }
    private val handler = Handler(thread.looper)
    private lateinit var display: EGLDisplay
    private lateinit var context: EGLContext
    private lateinit var eglSurface: EGLSurface
    private lateinit var texture: SurfaceTexture
    private var program = 0
    private var textureId = 0
    private val pacer = FramePacer()
    @Volatile private var released = false

    init {
        var failure: Throwable? = null
        val ready = CountDownLatch(1)
        handler.post {
            try { setUp(width, height, output) } catch (e: Throwable) { failure = e }
            ready.countDown()
        }
        if (!ready.await(5, TimeUnit.SECONDS)) failure = failure ?: IllegalStateException("GL setup timed out")
        failure?.let { release(); throw it }
        inputSurface = Surface(texture)
    }

    private fun setUp(width: Int, height: Int, output: Surface) {
        display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        val version = IntArray(2)
        check(EGL14.eglInitialize(display, version, 0, version, 1)) { "eglInitialize" }
        val attribs = intArrayOf(
            EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8, EGL14.EGL_BLUE_SIZE, 8,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT, EGL_RECORDABLE_ANDROID, 1, EGL14.EGL_NONE,
        )
        val configs = arrayOfNulls<EGLConfig>(1)
        val count = IntArray(1)
        check(EGL14.eglChooseConfig(display, attribs, 0, configs, 0, 1, count, 0) && count[0] > 0) { "eglChooseConfig" }
        context = EGL14.eglCreateContext(display, configs[0], EGL14.EGL_NO_CONTEXT,
            intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0)
        check(context != EGL14.EGL_NO_CONTEXT) { "eglCreateContext" }
        eglSurface = EGL14.eglCreateWindowSurface(display, configs[0], output, intArrayOf(EGL14.EGL_NONE), 0)
        check(eglSurface != EGL14.EGL_NO_SURFACE) { "eglCreateWindowSurface" }
        check(EGL14.eglMakeCurrent(display, eglSurface, eglSurface, context)) { "eglMakeCurrent" }

        program = link(VERTEX, FRAGMENT)
        val ids = IntArray(1)
        GLES20.glGenTextures(1, ids, 0)
        textureId = ids[0]
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, textureId)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glViewport(0, 0, width, height)

        texture = SurfaceTexture(textureId).apply {
            setDefaultBufferSize(width, height)
            setOnFrameAvailableListener({ draw() }, handler)
        }
    }

    private fun draw() {
        if (released) return
        try {
            // Always, or the camera stalls; its transform matrix is left out as it adds a rotation and crop the encoder ignores
            texture.updateTexImage()
            val ts = texture.timestamp
            if (!pacer.keep(ts, maxFps)) return
            GLES20.glUseProgram(program)
            val pos = GLES20.glGetAttribLocation(program, "aPos")
            GLES20.glEnableVertexAttribArray(pos)
            GLES20.glVertexAttribPointer(pos, 2, GLES20.GL_FLOAT, false, 0, QUAD)
            GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
            GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, textureId)
            GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
            EGLExt.eglPresentationTimeANDROID(display, eglSurface, ts)
            if (!EGL14.eglSwapBuffers(display, eglSurface)) throw IllegalStateException("eglSwapBuffers ${EGL14.eglGetError()}")
        } catch (e: Throwable) {
            if (!released) { released = true; onError(e) }
        }
    }

    fun release() {
        released = true
        val done = CountDownLatch(1)
        val posted = handler.post {
            try {
                if (::texture.isInitialized) texture.release()
                if (::display.isInitialized) {
                    EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
                    if (::eglSurface.isInitialized) EGL14.eglDestroySurface(display, eglSurface)
                    if (::context.isInitialized) EGL14.eglDestroyContext(display, context)
                    EGL14.eglTerminate(display)
                }
            } catch (_: Throwable) {}
            done.countDown()
        }
        if (posted) done.await(2, TimeUnit.SECONDS)
        if (::texture.isInitialized) try { inputSurface.release() } catch (_: Throwable) {}
        thread.quitSafely()
    }

    private fun link(vertex: String, fragment: String): Int {
        fun shader(type: Int, src: String) = GLES20.glCreateShader(type).also {
            GLES20.glShaderSource(it, src)
            GLES20.glCompileShader(it)
            val ok = IntArray(1)
            GLES20.glGetShaderiv(it, GLES20.GL_COMPILE_STATUS, ok, 0)
            check(ok[0] != 0) { "shader: ${GLES20.glGetShaderInfoLog(it)}" }
        }
        return GLES20.glCreateProgram().also {
            GLES20.glAttachShader(it, shader(GLES20.GL_VERTEX_SHADER, vertex))
            GLES20.glAttachShader(it, shader(GLES20.GL_FRAGMENT_SHADER, fragment))
            GLES20.glLinkProgram(it)
            val ok = IntArray(1)
            GLES20.glGetProgramiv(it, GLES20.GL_LINK_STATUS, ok, 0)
            check(ok[0] != 0) { "program: ${GLES20.glGetProgramInfoLog(it)}" }
        }
    }

    private companion object {
        const val EGL_RECORDABLE_ANDROID = 0x3142  // a surface MediaCodec can take
        val QUAD: java.nio.FloatBuffer = ByteBuffer.allocateDirect(8 * 4).order(ByteOrder.nativeOrder()).asFloatBuffer()
            .apply { put(floatArrayOf(-1f, -1f, 1f, -1f, -1f, 1f, 1f, 1f)); position(0) }
        const val VERTEX = """
            attribute vec2 aPos;
            varying vec2 vTex;
            void main() {
                gl_Position = vec4(aPos, 0.0, 1.0);
                vTex = vec2(aPos.x + 1.0, 1.0 - aPos.y) * 0.5;  // the buffer as it is, like the encoder reads it
            }"""
        const val FRAGMENT = """
            #extension GL_OES_EGL_image_external : require
            precision mediump float;
            varying vec2 vTex;
            uniform samplerExternalOES uTex0;
            void main() { gl_FragColor = texture2D(uTex0, vTex); }"""
    }
}

/** Which camera frames to keep for a rate: evenly spread (30 down to 24 keeps four of every five), by timestamp. */
class FramePacer {
    private var due = Long.MIN_VALUE  // when the next frame to keep is due, in the camera's nanoseconds

    fun keep(timestampNs: Long, maxFps: Int?): Boolean {
        if (maxFps == null || maxFps <= 0) { due = Long.MIN_VALUE; return true }
        val period = 1_000_000_000L / maxFps
        val slack = period / 4  // camera timestamps wobble; a frame a little early still counts
        if (due == Long.MIN_VALUE || timestampNs > due + period) due = timestampNs  // first, or after a gap
        if (timestampNs < due - slack) return false
        due += period
        return true
    }
}

