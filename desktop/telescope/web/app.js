// Telescope's Browser camera page: sends this device's camera (JPEG frames) and mic (48 kHz s16le) to the
// computer that served it, over one WebSocket. The token after # in the address is the computer's permission.
"use strict";

const FRAME_JPEG = 1;
const FRAME_PCM = 2;
const MAX_BUFFERED = 1_500_000;  // bytes waiting to go out; past this a frame is skipped instead of piling up lag
const JPEG_QUALITY = 0.8;

const token = decodeURIComponent(location.hash.slice(1));
const video = document.getElementById("video");
const statusEl = document.getElementById("status");
const dot = document.getElementById("dot");
const startBtn = document.getElementById("start");
const flipBtn = document.getElementById("flip");
const stopBtn = document.getElementById("stop");

let config = { width: 1280, height: 720, fps: 30, audio: false };
let facing = "user";
let media = null;
let micError = "";
let ws = null;
let running = false;
let retries = 0;
let retryTimer = null;
let frameTimer = null;
let encoding = false;
let audioCtx = null;
let wakeLock = null;
const canvas = document.createElement("canvas");
const ctx2d = canvas.getContext("2d");

function show(text, kind) {
  statusEl.textContent = text;
  statusEl.className = kind === "err" ? "err" : kind === "warn" ? "warn" : "";
  dot.className = kind || "";
}

function deviceName() {
  const ua = navigator.userAgent;
  const os = /iPhone/.test(ua) ? "iPhone" : /iPad/.test(ua) ? "iPad" : /Android/.test(ua) ? "Android"
    : /Mac/.test(ua) ? "Mac" : /Windows/.test(ua) ? "Windows" : /Linux/.test(ua) ? "Linux" : "";
  const browser = /EdgA?\//.test(ua) ? "Edge" : /Firefox|FxiOS/.test(ua) ? "Firefox"
    : /Chrome|CriOS/.test(ua) ? "Chrome" : /Safari/.test(ua) ? "Safari" : "a browser";
  return os ? `${browser} on ${os}` : browser;
}

// ── Camera and mic ─────────────────────────────────────────────────────────

function videoConstraints() {
  return { facingMode: facing, width: { ideal: config.width }, height: { ideal: config.height },
           frameRate: { ideal: config.fps } };
}

async function openMedia() {
  stopMedia();
  const audio = { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true };
  try {
    media = await navigator.mediaDevices.getUserMedia({ video: videoConstraints(), audio });
    micError = "";
  } catch (err) {
    if (err.name === "NotAllowedError" || err.name === "NotFoundError" || err.name === "NotReadableError") {
      // Maybe it's only the mic that's refused or missing: the camera alone still works.
      media = await navigator.mediaDevices.getUserMedia({ video: videoConstraints(), audio: false });
      micError = "The browser didn't allow the microphone.";
    } else {
      throw err;
    }
  }
  video.srcObject = media;
  video.classList.toggle("mirror", facing === "user");
  await video.play().catch(() => {});
  if (media.getAudioTracks().length) await startMic();
  for (const track of media.getTracks()) track.addEventListener("ended", onTrackEnded);
}

function stopMedia() {
  if (media) for (const track of media.getTracks()) track.stop();
  media = null;
  if (audioCtx) audioCtx.close().catch(() => {});
  audioCtx = null;
}

async function startMic() {
  try {
    try {
      audioCtx = new AudioContext({ sampleRate: 48000 });
    } catch (_) {
      audioCtx = new AudioContext();  // the worklet resamples whatever rate this runs at
    }
    await audioCtx.audioWorklet.addModule("/mic-worklet.js");
    const source = audioCtx.createMediaStreamSource(media);
    const node = new AudioWorkletNode(audioCtx, "mic-sender");
    node.port.onmessage = (e) => sendAudio(e.data);
    source.connect(node);
    // Some browsers only run a node that leads to the speakers; a muted gain keeps it silent.
    const mute = audioCtx.createGain();
    mute.gain.value = 0;
    node.connect(mute).connect(audioCtx.destination);
    await audioCtx.resume();
  } catch (err) {
    micError = "The browser couldn't record the microphone.";
  }
}

function onTrackEnded() {
  // The camera was taken away (another app, or the page went to the background on iOS); get it back when visible.
  if (running && document.visibilityState === "visible") restartMedia();
}

async function restartMedia() {
  try {
    await openMedia();
    sendHello();
  } catch (err) {
    show(cameraError(err), "err");
  }
}

function cameraError(err) {
  if (err && err.name === "NotAllowedError") return "The browser didn't allow the camera. Allow it in the browser's settings, then reload.";
  if (err && err.name === "NotFoundError") return "No camera found on this device.";
  if (!window.isSecureContext || !navigator.mediaDevices) return "This page needs HTTPS to use the camera.";
  return `Couldn't open the camera (${err && err.name || "unknown error"}).`;
}

// ── Sending ────────────────────────────────────────────────────────────────

function connect() {
  clearTimeout(retryTimer);
  const socket = new WebSocket(`wss://${location.host}/ws?token=${encodeURIComponent(token)}`);
  socket.binaryType = "arraybuffer";
  ws = socket;
  socket.onopen = () => {
    retries = 0;
    sendHello();
    show("Connected. Your computer can use this camera now.", "ok");
  };
  socket.onmessage = (e) => {
    if (typeof e.data !== "string") return;
    let msg;
    try { msg = JSON.parse(e.data); } catch (_) { return; }
    if (msg.type === "config") applyConfig(msg);
  };
  socket.onclose = (e) => {
    if (ws !== socket) return;
    ws = null;
    if (!running) return;
    if (e.code === 4000) {
      stop();
      show("This camera was opened on another device or tab.", "warn");
      return;
    }
    retries += 1;
    show(retries > 3 ? "Can't reach the computer. If its code changed, scan it again." : "Reconnecting…",
         retries > 3 ? "err" : "warn");
    retryTimer = setTimeout(connect, Math.min(5000, 500 * retries));
  };
}

function sendHello() {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({ type: "hello", device: deviceName(), mic_error: micError }));
  }
}

function applyConfig(msg) {
  const changed = msg.width !== config.width || msg.height !== config.height || msg.fps !== config.fps;
  config = { width: msg.width | 0 || 1280, height: msg.height | 0 || 720, fps: msg.fps | 0 || 30, audio: !!msg.audio };
  const track = media && media.getVideoTracks()[0];
  if (changed && track) track.applyConstraints(videoConstraints()).catch(() => {});
}

function sendFrame() {
  frameTimer = setTimeout(sendFrame, 1000 / config.fps);
  if (!ws || ws.readyState !== WebSocket.OPEN || encoding || ws.bufferedAmount > MAX_BUFFERED) return;
  const w = video.videoWidth, h = video.videoHeight;
  if (!w || !h) return;
  // Never more than the computer asked for; a camera that can't do that size sends what it has.
  const long = Math.max(config.width, config.height), short = Math.min(config.width, config.height);
  const scale = Math.min(1, long / Math.max(w, h), short / Math.min(w, h));
  canvas.width = Math.round(w * scale);
  canvas.height = Math.round(h * scale);
  ctx2d.drawImage(video, 0, 0, canvas.width, canvas.height);
  encoding = true;
  canvas.toBlob((blob) => {
    encoding = false;
    if (blob && ws && ws.readyState === WebSocket.OPEN) ws.send(new Blob([new Uint8Array([FRAME_JPEG]), blob]));
  }, "image/jpeg", JPEG_QUALITY);
}

function sendAudio(buffer) {
  if (!config.audio || !ws || ws.readyState !== WebSocket.OPEN || ws.bufferedAmount > MAX_BUFFERED) return;
  const out = new Uint8Array(buffer.byteLength + 1);
  out[0] = FRAME_PCM;
  out.set(new Uint8Array(buffer), 1);
  ws.send(out);
}

// ── Screen ─────────────────────────────────────────────────────────────────

async function keepAwake() {
  try {
    if ("wakeLock" in navigator && document.visibilityState === "visible") {
      wakeLock = await navigator.wakeLock.request("screen");
    }
  } catch (_) {
    wakeLock = null;  // not offered here (low battery, older browser); the note asks to keep the screen on
  }
}

document.addEventListener("visibilitychange", () => {
  if (!running || document.visibilityState !== "visible") return;
  keepAwake();
  const track = media && media.getVideoTracks()[0];
  if (!track || track.readyState === "ended") restartMedia();
  if (audioCtx && audioCtx.state !== "running") audioCtx.resume().catch(() => {});
  if (!ws) connect();
});

// ── Buttons ────────────────────────────────────────────────────────────────

async function start() {
  if (!token) {
    show("This address is missing its code. Scan the code on the computer again.", "err");
    return;
  }
  if (!window.isSecureContext || !navigator.mediaDevices) {
    show(cameraError(null), "err");
    return;
  }
  startBtn.disabled = true;
  show("Opening the camera…");
  try {
    await openMedia();
  } catch (err) {
    startBtn.disabled = false;
    show(cameraError(err), "err");
    return;
  }
  running = true;
  startBtn.hidden = true;
  flipBtn.hidden = stopBtn.hidden = false;
  keepAwake();
  show("Connecting…");
  connect();
  sendFrame();
}

function stop() {
  running = false;
  clearTimeout(retryTimer);
  clearTimeout(frameTimer);
  const socket = ws;
  ws = null;
  if (socket) socket.close(1000);
  stopMedia();
  if (wakeLock) wakeLock.release().catch(() => {});
  wakeLock = null;
  video.srcObject = null;
  startBtn.hidden = false;
  startBtn.disabled = false;
  flipBtn.hidden = stopBtn.hidden = true;
  show("Stopped. Tap Start to use this camera again.");
}

startBtn.addEventListener("click", start);
stopBtn.addEventListener("click", stop);
flipBtn.addEventListener("click", async () => {
  facing = facing === "user" ? "environment" : "user";
  flipBtn.disabled = true;
  await restartMedia();
  flipBtn.disabled = false;
});

if (!token) show("This address is missing its code. Scan the code on the computer again.", "err");
