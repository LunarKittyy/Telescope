// Telescope's Browser camera page: sends this device's camera (H.264 from its hardware encoder, else JPEG) and mic
// (48 kHz s16le) to the computer that served it, over one WebSocket. The token after # in the address is the computer's permission.
"use strict";

const FRAME_JPEG = 1;
const FRAME_PCM = 2;
const FRAME_H264 = 3;
const MAX_BUFFERED = 1_500_000;  // bytes waiting to go out; past this a frame is skipped instead of piling up lag
const JPEG_QUALITY = 0.8;
const STEP_DOWN = [1280, 854];  // long edges to fall back to when this device can't encode frames as fast as asked
const SLOW_SECONDS = 2;  // how often to look at whether encoding keeps up
const SLOW_SHARE = 0.25;  // frames skipped for a busy encoder past which it steps down
const H264_CODECS = ["avc1.42E028", "avc1.4D0028"];  // Constrained Baseline, then Main, level 4.0 (1080p30)
const KEYFRAME_SECONDS = 2;
const HINT_SECONDS = 4;  // how long the black screen's hint stays, and how long a first tap waits for the second
const WAKE_GUARD_MS = 500;  // buttons ignore taps this soon after a double tap wakes the screen (a third tap)
const PLAYER_STALL_SECONDS = 3;  // iPhone: a camera preview frozen this long behind the black video player closes the player
const LOCK_RETRY_SECONDS = 5;  // a wake lock dropped sooner than this isn't asked for again until the page is shown again

const token = decodeURIComponent(location.hash.slice(1));
const browserId = keptId();  // tells the computer it's this browser again (a reload), not another camera
const video = document.getElementById("video");
const statusEl = document.getElementById("status");
const dot = document.getElementById("dot");
const startBtn = document.getElementById("start");
const flipBtn = document.getElementById("flip");
const stopBtn = document.getElementById("stop");
const darkBtn = document.getElementById("dark");
const black = document.getElementById("black");
const wakeHint = document.getElementById("wake-hint");
const blackStatus = document.getElementById("black-status");

let config = { width: 1280, height: 720, fps: 30, audio: false };
let facing = "user";
let media = null;
let mediaGen = 0;  // bumped by each open and by Stop, so a camera that opens late knows it's no longer wanted
let runGen = 0;  // bumped by Stop, so a frame still encoding from before it isn't sent on the next Start's connection
let micError = "";
let ws = null;
let running = false;
let retries = 0;
let retryTimer = null;
let frameTimer = null;
let encoding = false;  // a JPEG is being made
let ticks = 0, skipped = 0, paceSince = 0;
let longCap = Infinity;  // the long edge this device keeps up with, once it has had to step down
let encoder = null;
let encoderSize = "";  // what encoder is set up for
let configuring = false;
let h264Off = "";  // why this page sends JPEG though the computer takes H.264
let needKey = true;
let dropping = false;  // a chunk didn't go out, so the frames after it wait for a keyframe
let sinceKey = 0;
let codecNow = "", codecNote = "";
let audioCtx = null;
let wakeLock = null;
let hintTimer = null;
let tapped = false;  // one tap on the black screen, waiting for the second
let wokeAt = -Infinity;
let blackVideo = null;  // iPhone: a black video shown in the system player, since pages there can't go full screen
let blackPlayed = false;  // the black video has played at least once, so it works here
let playerOff = false;  // the player stalled the camera here once, so this page load sticks to the plain black page
let leavingPlayer = false;  // the page closed the player itself and stays black
let playerTimer = null;
const canvas = document.createElement("canvas");
const ctx2d = canvas.getContext("2d");

function keptId() {
  const make = () => Array.from(crypto.getRandomValues(new Uint8Array(12)), (b) => b.toString(16).padStart(2, "0")).join("");
  for (const store of [() => localStorage, () => sessionStorage]) {
    try {
      const s = store();
      let id = s.getItem("telescope-browser-id");
      if (!id || !/^[A-Za-z0-9_-]{8,40}$/.test(id)) {
        id = make();
        s.setItem("telescope-browser-id", id);
      }
      return id;
    } catch (_) { /* storage blocked: try the next, else this page load only */ }
  }
  return make();
}

function showConnected() {
  show(config.camera ? `Connected. Your computer streams this camera as ${config.camera}.`
                     : "Connected. Your computer can use this camera now.", "ok");
}

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

// Returns false when Stop or a newer open took over while this one waited for the camera.
async function openMedia() {
  stopMedia();
  const gen = ++mediaGen;
  const stale = (got) => {
    if (gen === mediaGen) return false;
    for (const track of got.getTracks()) track.stop();
    return true;
  };
  const audio = { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true };
  let got;
  try {
    got = await navigator.mediaDevices.getUserMedia({ video: videoConstraints(), audio });
    micError = "";
  } catch (err) {
    if (gen !== mediaGen) return false;
    if (err.name === "NotAllowedError" || err.name === "NotFoundError" || err.name === "NotReadableError") {
      // Maybe it's only the mic that's refused or missing: the camera alone still works.
      got = await navigator.mediaDevices.getUserMedia({ video: videoConstraints(), audio: false });
      micError = "The browser didn't allow the microphone.";
    } else {
      throw err;
    }
  }
  if (stale(got)) return false;
  media = got;
  video.srcObject = media;
  video.classList.toggle("mirror", facing === "user");
  await video.play().catch(() => {});
  if (stale(got)) return false;
  if (media.getAudioTracks().length) await startMic();
  if (stale(got)) return false;
  for (const track of media.getTracks()) track.addEventListener("ended", onTrackEnded);
  return true;
}

function stopMedia() {
  if (media) for (const track of media.getTracks()) track.stop();
  media = null;
  if (audioCtx) audioCtx.close().catch(() => {});
  audioCtx = null;
}

async function startMic() {
  const gen = mediaGen, stream = media;  // this setup's own: a Flip or a Stop and Start while it waits hands the globals on
  try {
    let ctx;
    try {
      ctx = new AudioContext({ sampleRate: 48000 });
    } catch (_) {
      ctx = new AudioContext();  // the worklet resamples whatever rate this runs at
    }
    audioCtx = ctx;
    await ctx.audioWorklet.addModule("/mic-worklet.js");
    if (gen !== mediaGen) {
      ctx.close().catch(() => {});
      return;
    }
    const source = ctx.createMediaStreamSource(stream);
    const node = new AudioWorkletNode(ctx, "mic-sender");
    node.port.onmessage = (e) => sendAudio(e.data);
    source.connect(node);
    // Some browsers only run a node that leads to the speakers; a muted gain keeps it silent.
    const mute = ctx.createGain();
    mute.gain.value = 0;
    node.connect(mute).connect(ctx.destination);
    await ctx.resume();
  } catch (err) {
    if (gen === mediaGen) micError = "The browser couldn't record the microphone.";
  }
}

function onTrackEnded() {
  // The camera was taken away (another app, or the page went to the background on iOS); get it back when visible.
  if (running && document.visibilityState === "visible") restartMedia();
}

async function restartMedia() {
  try {
    if (await openMedia()) sendHello();
  } catch (err) {
    if (running) show(cameraError(err), "err");
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
  const socket = new WebSocket(
    `wss://${location.host}/ws?token=${encodeURIComponent(token)}&id=${encodeURIComponent(browserId)}`);
  socket.binaryType = "arraybuffer";
  ws = socket;
  socket.onopen = () => {
    retries = 0;
    needKey = true;  // the computer starts a new decoder
    sendHello();
    showConnected();
  };
  socket.onmessage = (e) => {
    if (typeof e.data !== "string") return;
    let msg;
    try { msg = JSON.parse(e.data); } catch (_) { return; }
    if (msg.type === "config") applyConfig(msg);
    else if (msg.type === "keyframe") needKey = true;
  };
  socket.onclose = (e) => {
    if (ws !== socket) return;
    ws = null;
    if (!running) return;
    if (e.code === 4000) {
      stop();
      show("This camera was opened in another tab.", "warn");
      return;
    }
    if (e.code === 4001) {
      stop();
      show("All of your computer's virtual cameras are in use. Stop one there, then tap Start.", "warn");
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
    ws.send(JSON.stringify({ type: "hello", device: deviceName(), mic_error: micError, codec: codecNow,
                             codec_note: codecNote }));
  }
}

function applyConfig(msg) {
  const changed = msg.width !== config.width || msg.height !== config.height || msg.fps !== config.fps;
  const named = (msg.camera || "") !== (config.camera || "");
  config = { width: msg.width | 0 || 1280, height: msg.height | 0 || 720, fps: msg.fps | 0 || 30, audio: !!msg.audio,
             h264: !!msg.h264, camera: typeof msg.camera === "string" ? msg.camera : "" };
  if (named && ws && ws.readyState === WebSocket.OPEN) showConnected();
  if (changed) {
    longCap = Infinity;  // a size picked on the computer gets another try
    resetPace();
  }
  if (!config.h264) closeEncoder();
  const track = media && media.getVideoTracks()[0];
  if (changed && track) track.applyConstraints(videoConstraints()).catch(() => {});
}

function sendFrame() {
  frameTimer = setTimeout(sendFrame, 1000 / config.fps);
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  const w = video.videoWidth, h = video.videoHeight;
  if (!w || !h) return;
  const [fw, fh] = frameSize(w, h);
  const h264 = wantH264();
  if (h264 && !encoderReady(fw, fh)) return;
  if (!h264) showCodec("jpeg", h264Off || (config.h264 ? "" : "The computer can't decode H.264, so this sends JPEG."));
  const busy = h264 ? encoder.encodeQueueSize > 1 : encoding;
  notePace(busy);
  if (busy || ws.bufferedAmount > MAX_BUFFERED) return;
  canvas.width = fw;
  canvas.height = fh;
  ctx2d.drawImage(video, 0, 0, fw, fh);
  if (h264) encodeH264();
  else encodeJpeg();
}

function frameSize(w, h) {
  // Never more than the computer asked for; a camera that can't do that size sends what it has.
  const askedLong = Math.max(config.width, config.height), askedShort = Math.min(config.width, config.height);
  const long = Math.min(askedLong, longCap), short = askedShort * long / askedLong;
  const scale = Math.min(1, long / Math.max(w, h), short / Math.min(w, h));
  return [2 * Math.round(w * scale / 2), 2 * Math.round(h * scale / 2)];  // H.264 needs even sizes
}

function encodeJpeg() {
  encoding = true;
  const gen = runGen;
  canvas.toBlob((blob) => {
    if (gen !== runGen) return;  // stopped since: Stop already cleared encoding for the next run
    encoding = false;
    if (blob && ws && ws.readyState === WebSocket.OPEN) ws.send(new Blob([new Uint8Array([FRAME_JPEG]), blob]));
  }, "image/jpeg", JPEG_QUALITY);
}

// ── H.264 ──────────────────────────────────────────────────────────────────

function wantH264() {
  if (!config.h264 || h264Off) return false;
  if (typeof VideoEncoder !== "function" || typeof VideoFrame !== "function") {
    useJpeg("This browser has no video encoder for pages, so this sends JPEG.");
    return false;
  }
  return true;
}

function encoderReady(w, h) {
  const size = `${w}x${h}@${config.fps}`;
  if (encoder && encoder.state === "configured" && encoderSize === size) return true;
  if (!configuring) setUpEncoder(w, h, size);
  return false;
}

async function setUpEncoder(w, h, size) {
  configuring = true;
  const gen = runGen;
  try {
    const cfg = await hardwareConfig(w, h);
    if (gen !== runGen) return;  // stopped while the browser was checking: nothing here is wanted any more
    if (!cfg) {
      // Some hardware encoders stop short of 1080p: a smaller size may still have one
      const next = STEP_DOWN.find((l) => l < Math.max(w, h));
      if (next) longCap = next;
      else useJpeg("This device has no hardware H.264 encoder the browser can use, so this sends JPEG.");
      return;
    }
    if (!encoder || encoder.state === "closed") {
      const enc = new VideoEncoder({
        output: sendChunk,
        error: (err) => { if (enc === encoder) useJpeg(`The H.264 encoder stopped (${err.message}), so this sends JPEG.`); },
      });
      encoder = enc;
    }
    encoder.configure(cfg);
    encoderSize = size;
    needKey = true;
    showCodec("h264", "");
  } catch (err) {
    if (gen === runGen) useJpeg(`H.264 didn't start (${err.message}), so this sends JPEG.`);
  } finally {
    configuring = false;
  }
}

async function hardwareConfig(w, h) {
  for (const codec of H264_CODECS) {
    // Chrome takes prefer-hardware as hardware only; a software encoder would be as slow as JPEG on a phone
    const cfg = { codec, width: w, height: h, framerate: config.fps, bitrate: Math.min(12e6, Math.round(w * h * config.fps * 0.12)),
                  hardwareAcceleration: "prefer-hardware", latencyMode: "realtime", avc: { format: "annexb" } };
    try {
      if ((await VideoEncoder.isConfigSupported(cfg)).supported) return cfg;
    } catch (_) {
      // not this one
    }
  }
  return null;
}

function encodeH264() {
  const frame = new VideoFrame(canvas, { timestamp: Math.round(performance.now() * 1000) });
  const key = needKey || ++sinceKey >= config.fps * KEYFRAME_SECONDS;
  if (key) {
    needKey = false;
    sinceKey = 0;
  }
  try {
    encoder.encode(frame, { keyFrame: key });
  } catch (err) {
    useJpeg(`The H.264 encoder stopped (${err.message}), so this sends JPEG.`);
  } finally {
    frame.close();
  }
}

function sendChunk(chunk) {
  const key = chunk.type === "key";
  if (!ws || ws.readyState !== WebSocket.OPEN || ws.bufferedAmount > MAX_BUFFERED) {
    dropping = needKey = true;  // what comes next refers to this frame, so start over from a keyframe
    return;
  }
  if (dropping && !key) return;
  dropping = false;
  const out = new Uint8Array(chunk.byteLength + 2);
  out[0] = FRAME_H264;
  out[1] = key ? 1 : 0;
  chunk.copyTo(out.subarray(2));
  ws.send(out);
}

function useJpeg(note) {
  h264Off = note;
  closeEncoder();
  showCodec("jpeg", note);
}

function closeEncoder() {
  if (encoder && encoder.state !== "closed") {
    try { encoder.close(); } catch (_) { /* already gone */ }
  }
  encoder = null;
  encoderSize = "";
}

function showCodec(codec, note) {
  if (codec === codecNow && note === codecNote) return;
  codecNow = codec;
  codecNote = note;
  sendHello();
}

// Every couple of seconds: a device that skipped more than a share of frames for a busy encoder steps down a size.
function notePace(busy) {
  const now = performance.now();
  if (!paceSince) paceSince = now;
  ticks += 1;
  if (busy) skipped += 1;
  if (now - paceSince < SLOW_SECONDS * 1000) return;
  const slow = skipped > ticks * SLOW_SHARE;
  resetPace();
  const next = STEP_DOWN.find((l) => l < Math.max(canvas.width, canvas.height));
  if (slow && next) longCap = next;
}

function resetPace() {
  ticks = skipped = paceSince = 0;
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
      const lock = await navigator.wakeLock.request("screen");
      const since = performance.now();
      lock.addEventListener("release", () => {
        if (wakeLock !== lock) return;
        wakeLock = null;
        const lasted = performance.now() - since > LOCK_RETRY_SECONDS * 1000;  // else a browser taking it straight back would loop
        if (running && lasted && document.visibilityState === "visible") keepAwake();  // the browser let go of it (battery saver)
      });
      if (running) wakeLock = lock;
      else lock.release().catch(() => {});  // Stop came while it was being granted
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

// Black screen: everything off but the camera, with the phone's bars hidden too where the browser allows it.
function pageFullscreen() {
  return !!(document.fullscreenEnabled || document.webkitFullscreenEnabled);
}

// Played (muted, tiny, invisible) from Start on to be ready within the Black screen tap; anything unexpected leaves it off.
function prepareBlackVideo() {
  if (pageFullscreen() || typeof video.webkitEnterFullscreen !== "function") return;
  if (blackVideo) {
    blackVideo.play().catch(() => {});
    return;
  }
  let v = null;
  try {
    const frame = document.createElement("canvas");
    if (typeof frame.captureStream !== "function") return;
    frame.width = frame.height = 16;
    const stream = frame.captureStream(1);
    const g = frame.getContext("2d");
    g.fillStyle = "#000";
    g.fillRect(0, 0, 16, 16);
    v = document.createElement("video");
    if (typeof v.webkitEnterFullscreen !== "function") return;
    v.muted = true;
    v.playsInline = true;
    v.setAttribute("playsinline", "");
    v.className = "black-video";
    v.srcObject = stream;
    v.addEventListener("webkitendfullscreen", onPlayerClosed);
    v.addEventListener("playing", () => { blackPlayed = true; });
    v.addEventListener("pause", () => { if (v.webkitDisplayingFullscreen) v.play().catch(() => {}); });  // paused, the player keeps its controls up
    document.body.appendChild(v);
    v.play().catch(() => {});
    blackVideo = v;
  } catch (_) {
    if (v && v.parentNode) v.parentNode.removeChild(v);
  }
}

function playerReady() {
  return !!blackVideo && !playerOff && blackPlayed && blackVideo.readyState >= 2;
}

function onPlayerClosed() {
  clearTimeout(playerTimer);
  if (leavingPlayer) {
    leavingPlayer = false;
    if (!black.hidden) showHint();
    return;
  }
  if (!black.hidden) wake();  // closing the player is the double tap here
}

// While the player is up, the camera preview has to keep moving; if it stalls, the player goes and doesn't come back.
function watchPlayer(last, still) {
  playerTimer = setTimeout(() => {
    if (!blackVideo || !blackVideo.webkitDisplayingFullscreen) return;
    const now = video.currentTime;
    if (now !== last) {
      watchPlayer(now, 0);
      return;
    }
    if (still + 1 < PLAYER_STALL_SECONDS) {
      watchPlayer(last, still + 1);
      return;
    }
    playerOff = true;
    leavingPlayer = true;
    try { blackVideo.webkitExitFullscreen(); } catch (_) { leavingPlayer = false; }
  }, 1000);
}

function goDark() {
  black.hidden = false;
  tapped = false;
  showHint();
  const root = document.documentElement;
  const enter = root.requestFullscreen || root.webkitRequestFullscreen;
  if (enter && pageFullscreen()) {
    Promise.resolve(enter.call(root)).catch(() => {});
  } else if (playerReady()) {
    try {
      blackVideo.play().catch(() => {});  // iOS pauses a video when its player closes
      blackVideo.webkitEnterFullscreen();
      watchPlayer(video.currentTime, 0);
    } catch (_) { /* refused: the black page still works */ }
  }
}

function wake() {
  black.hidden = true;
  tapped = false;
  clearTimeout(hintTimer);
  clearTimeout(playerTimer);
  if (blackVideo && blackVideo.webkitDisplayingFullscreen) {
    try { blackVideo.webkitExitFullscreen(); } catch (_) { /* already closing */ }
  }
  const leave = document.exitFullscreen || document.webkitExitFullscreen;
  if (leave && (document.fullscreenElement || document.webkitFullscreenElement)) {
    Promise.resolve(leave.call(document)).catch(() => {});
  }
}

function showHint() {
  blackStatus.textContent = statusEl.textContent;
  wakeHint.classList.toggle("faded", false);
  clearTimeout(hintTimer);
  hintTimer = setTimeout(() => {
    tapped = false;
    wakeHint.classList.toggle("faded", true);
  }, HINT_SECONDS * 1000);
}

const justWoke = () => performance.now() - wokeAt < WAKE_GUARD_MS;

black.addEventListener("click", () => {
  if (tapped) {
    wokeAt = performance.now();
    wake();
    return;
  }
  tapped = true;
  showHint();
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
    if (!await openMedia()) return;
  } catch (err) {
    startBtn.disabled = false;
    show(cameraError(err), "err");
    return;
  }
  running = true;
  prepareBlackVideo();
  startBtn.hidden = true;
  flipBtn.hidden = stopBtn.hidden = darkBtn.hidden = false;
  keepAwake();
  show("Connecting…");
  connect();
  sendFrame();
}

function stop() {
  running = false;
  mediaGen++;
  runGen++;
  clearTimeout(retryTimer);
  clearTimeout(frameTimer);
  const socket = ws;
  ws = null;
  if (socket) socket.close(1000);
  stopMedia();
  closeEncoder();
  h264Off = codecNow = codecNote = "";
  encoding = false;
  if (wakeLock) wakeLock.release().catch(() => {});
  wakeLock = null;
  video.srcObject = null;
  startBtn.hidden = false;
  startBtn.disabled = false;
  flipBtn.hidden = stopBtn.hidden = darkBtn.hidden = true;
  wake();
  if (blackVideo) blackVideo.pause();
  show("Stopped. Tap Start to use this camera again.");
}

startBtn.addEventListener("click", start);
stopBtn.addEventListener("click", () => { if (!justWoke()) stop(); });
darkBtn.addEventListener("click", () => { if (!justWoke()) goDark(); });
flipBtn.addEventListener("click", async () => {
  if (justWoke()) return;
  facing = facing === "user" ? "environment" : "user";
  flipBtn.disabled = true;
  await restartMedia();
  flipBtn.disabled = false;
});

if (!token) show("This address is missing its code. Scan the code on the computer again.", "err");
