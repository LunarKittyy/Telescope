"use strict";
// The 3D headset page: JPEG pictures (both eyes side by side) arrive over a WebSocket and are shown flat, or one half to each eye in VR.

const token = location.hash.slice(1);
const flat = document.getElementById("flat");
const flatContext = flat.getContext("2d");
const enter = document.getElementById("enter");
const statusLine = document.getElementById("status");

// What is shown: the newest decoded picture, and whether it is new to the screen. Blank until a picture comes.
let bitmap = null;
let fresh = false;
let socket = null;
let pictures = false;
let session = null;
let gl = null;
let glState = null;

function say(text) {
  statusLine.textContent = text;
}

function show(next) {
  if (bitmap && bitmap !== next) bitmap.close();
  bitmap = next;
  fresh = true;
  pictures = next !== null;
  if (!session) say(pictures ? "Connected" : "Connected, waiting for the 3D picture");
}

// Decodes one picture at a time, off the main thread; a picture that arrives meanwhile replaces the one waiting.
let decoding = false;
let waiting = null;
// Bumped when the picture is blanked or the socket closes: a decode that started before it must not bring the old picture back.
let generation = 0;

function blank() {
  generation++;
  waiting = null;
  show(null);
}

async function decode(blob) {
  if (decoding) {
    waiting = blob;
    return;
  }
  decoding = true;
  const started = generation;
  try {
    const decoded = await createImageBitmap(blob);
    if (started === generation) show(decoded);
    else decoded.close();
  } catch (e) {
    // a damaged picture is skipped
  }
  decoding = false;
  if (waiting) {
    const next = waiting;
    waiting = null;
    decode(next);
  }
}

function connect() {
  const url = "wss://" + location.host + "/ws?token=" + encodeURIComponent(token);
  socket = new WebSocket(url);
  socket.binaryType = "blob";
  socket.onopen = () => {
    if (!session) say("Connected, waiting for the 3D picture");
  };
  socket.onmessage = (event) => {
    if (typeof event.data === "string") {
      blank();
    } else {
      decode(event.data);
    }
  };
  socket.onclose = () => {
    // the last picture doesn't stay up while Telescope is away
    blank();
    if (!session) say("Waiting for Telescope…");
    setTimeout(connect, 1500);
  };
}

// ── Flat ──

function drawFlat() {
  if (fresh && !session) {
    fresh = false;
    if (bitmap) {
      if (flat.width !== bitmap.width || flat.height !== bitmap.height) {
        flat.width = bitmap.width;
        flat.height = bitmap.height;
      }
      flatContext.drawImage(bitmap, 0, 0);
    } else {
      flatContext.fillStyle = "#000";
      flatContext.fillRect(0, 0, flat.width, flat.height);
    }
  }
  if (!session) requestAnimationFrame(drawFlat);
}

// ── VR ──

const VERTEX = "attribute vec2 p; uniform mat4 mvp; uniform vec2 shift; varying vec2 uv;" +
  "void main() { uv = vec2(shift.x + (p.x + 0.5) * 0.5, 0.5 - p.y); gl_Position = mvp * vec4(p, 0.0, 1.0); }";
const FRAGMENT = "precision highp float; uniform sampler2D tex; varying vec2 uv; void main() { gl_FragColor = texture2D(tex, uv); }";
// The picture hangs 2 m in front, about 2.5 m wide at 16:9 for each eye
const DISTANCE = 2;
const WIDTH = 2.5;
const HEIGHT = WIDTH * 9 / 16;

function compile(type, source) {
  const shader = gl.createShader(type);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  return shader;
}

function setUpGl() {
  const program = gl.createProgram();
  gl.attachShader(program, compile(gl.VERTEX_SHADER, VERTEX));
  gl.attachShader(program, compile(gl.FRAGMENT_SHADER, FRAGMENT));
  gl.linkProgram(program);
  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-0.5, -0.5, 0.5, -0.5, -0.5, 0.5, 0.5, 0.5]), gl.STATIC_DRAW);
  const texture = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  glState = {
    program, texture, buffer, size: [0, 0],
    p: gl.getAttribLocation(program, "p"), mvp: gl.getUniformLocation(program, "mvp"), shift: gl.getUniformLocation(program, "shift"),
  };
}

// Puts the picture in the texture, made again only when its size changes
function upload() {
  if (!fresh) return;
  fresh = false;
  if (!bitmap) return;
  gl.bindTexture(gl.TEXTURE_2D, glState.texture);
  if (glState.size[0] !== bitmap.width || glState.size[1] !== bitmap.height) {
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, bitmap.width, bitmap.height, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    glState.size = [bitmap.width, bitmap.height];
  }
  gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, bitmap);
}

// Column-major 4x4 product a * b
function multiply(a, b) {
  const out = new Float32Array(16);
  for (let c = 0; c < 4; c++) {
    for (let r = 0; r < 4; r++) {
      let sum = 0;
      for (let k = 0; k < 4; k++) sum += a[k * 4 + r] * b[c * 4 + k];
      out[c * 4 + r] = sum;
    }
  }
  return out;
}

// The picture's own place: 2 m ahead, scaled to its size
const MODEL = new Float32Array([WIDTH, 0, 0, 0, 0, HEIGHT, 0, 0, 0, 0, 1, 0, 0, 0, -DISTANCE, 1]);

function onFrame(time, frame) {
  session.requestAnimationFrame(onFrame);
  const layer = session.renderState.baseLayer;
  const pose = frame.getViewerPose(glState.space);
  gl.bindFramebuffer(gl.FRAMEBUFFER, layer.framebuffer);
  gl.clearColor(0, 0, 0, 1);
  gl.clear(gl.COLOR_BUFFER_BIT);
  if (!pose) return;
  upload();
  const ready = bitmap && glState.size[0] > 0;
  pose.views.forEach((view, index) => {
    const viewport = layer.getViewport(view);
    gl.viewport(viewport.x, viewport.y, viewport.width, viewport.height);
    if (!ready) return;
    const right = view.eye === "right" || (view.eye === "none" && index === 1);
    gl.useProgram(glState.program);
    gl.uniformMatrix4fv(glState.mvp, false, multiply(multiply(view.projectionMatrix, view.transform.inverse.matrix), MODEL));
    gl.uniform2f(glState.shift, right ? 0.5 : 0, 0);
    gl.bindTexture(gl.TEXTURE_2D, glState.texture);
    gl.bindBuffer(gl.ARRAY_BUFFER, glState.buffer);
    gl.enableVertexAttribArray(glState.p);
    gl.vertexAttribPointer(glState.p, 2, gl.FLOAT, false, 0, 0);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  });
}

async function enterVr() {
  enter.disabled = true;
  try {
    const canvas = document.createElement("canvas");
    gl = canvas.getContext("webgl", { xrCompatible: true, antialias: false });
    setUpGl();
    session = await navigator.xr.requestSession("immersive-vr");
    glState.space = await session.requestReferenceSpace("local");
    session.updateRenderState({ baseLayer: new XRWebGLLayer(session, gl, { antialias: false }) });
    session.addEventListener("end", () => {
      session = null;
      enter.disabled = false;
      fresh = true;
      say(pictures ? "Connected" : "Waiting for the 3D picture");
      drawFlat();
    });
    fresh = true;
    session.requestAnimationFrame(onFrame);
  } catch (e) {
    session = null;
    enter.disabled = false;
    say("Couldn't start VR");
  }
}

enter.addEventListener("click", enterVr);

if (navigator.xr && navigator.xr.isSessionSupported) {
  navigator.xr.isSessionSupported("immersive-vr").then((ok) => {
    enter.disabled = !ok;
    if (!ok) say("VR isn't available in this browser");
  }, () => {});
}

drawFlat();
connect();
