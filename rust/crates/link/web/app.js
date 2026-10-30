// Telescope's Browser camera page: sends this device's camera and mic to the computer that served it over WebRTC.
// A WebSocket to the computer carries the offer and answer, the settings and stats. The token after # in the address is the computer's permission.
"use strict";

const HINT_SECONDS = 4;  // how long the black screen's hint stays, and how long a first tap waits for the second
const WAKE_GUARD_MS = 500;  // buttons ignore taps this soon after a double tap wakes the screen (a third tap)
const PLAYER_STALL_SECONDS = 3;  // iPhone: a camera preview frozen this long behind the black video player closes the player
const LOCK_RETRY_SECONDS = 5;  // a wake lock dropped sooner than this isn't asked for again until the page is shown again
const ICE_WAIT_MS = 2500;  // the offer goes with every address found by then; on a home network that's well under a second
const STATS_SECONDS = 2;
const DISCONNECTED_SECONDS = 5;  // a connection that says disconnected this long is set up again
// A raw mic, like a USB one: the app on the computer does its own echo cancellation, noise suppression and levels
const MIC = { channelCount: 1, echoCancellation: false, noiseSuppression: false, autoGainControl: false };

const token = decodeURIComponent(location.hash.slice(1));
const browserId = keptId();  // tells the computer it's this browser again (a reload), not another camera
const video = document.getElementById("video");
const statusEl = document.getElementById("status");
const rateEl = document.getElementById("rate");
const dot = document.getElementById("dot");
const startBtn = document.getElementById("start");
const flipBtn = document.getElementById("flip");
const stopBtn = document.getElementById("stop");
const darkBtn = document.getElementById("dark");
const lightBtn = document.getElementById("light");
const screenLight = document.getElementById("screen-light");
const black = document.getElementById("black");
const wakeHint = document.getElementById("wake-hint");
const blackStatus = document.getElementById("black-status");

let config = { width: 1280, height: 720, fps: 30, audio: false, max_kbps: 4000, camera: "" };
let facing = "user";
let camera = null;  // the video track
let mic = null;  // the audio track, kept apart from the camera's so a Flip or a restart never cuts it
let wantMic = false;  // the mic was allowed, so it's opened again after the browser takes it away
let torchOn = false;
let mediaGen = 0;  // bumped by each open and by Stop, so a camera that opens late knows it's no longer wanted
let micError = "";
let ws = null;
let running = false;
let retries = 0;
let retryTimer = null;
let pc = null;
let videoSender = null;
let micSender = null;
let downTimer = null;  // a disconnected peer connection gets this long to come back
let statsTimer = null;
let wakeLock = null;
let hintTimer = null;
let tapped = false;  // one tap on the black screen, waiting for the second
let wokeAt = -Infinity;
let blackVideo = null;  // iPhone: a black video shown in the system player, since pages there can't go full screen
let blackPlayed = false;  // the black video has played at least once, so it works here
let playerOff = false;  // the player stalled the camera here once, so this page load sticks to the plain black page
let leavingPlayer = false;  // the page closed the player itself and stays black
let playerTimer = null;

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
  if (kind !== "ok") rateEl.textContent = "";
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
  return { facingMode: { ideal: facing }, width: { ideal: config.width }, height: { ideal: config.height },
           frameRate: { ideal: config.fps }, resizeMode: "crop-and-scale" };
}

// The first open: camera and mic in one go, so the browser asks once. Returns false when Stop or a newer open took over.
async function openMedia() {
  stopMedia();
  const gen = ++mediaGen;
  let got;
  try {
    got = await navigator.mediaDevices.getUserMedia({ video: videoConstraints(), audio: MIC });
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
  if (gen !== mediaGen) {
    for (const track of got.getTracks()) track.stop();
    return false;
  }
  const audio = got.getAudioTracks()[0];
  wantMic = !!audio;
  if (audio) useMic(audio);
  useCamera(got.getVideoTracks()[0]);
  return true;
}

// Flip, a size the camera ignored, or the camera coming back: the camera alone, the mic carries on.
async function openCamera() {
  stopCamera();
  const gen = ++mediaGen;
  const got = await navigator.mediaDevices.getUserMedia({ video: videoConstraints(), audio: false });
  if (gen !== mediaGen) {
    for (const track of got.getTracks()) track.stop();
    return false;
  }
  useCamera(got.getVideoTracks()[0]);
  return true;
}

async function openMic() {
  const gen = mediaGen;
  try {
    const got = await navigator.mediaDevices.getUserMedia({ audio: MIC, video: false });
    if (gen !== mediaGen || !running) {
      for (const track of got.getTracks()) track.stop();
      return;
    }
    useMic(got.getAudioTracks()[0]);
    micError = "";
    sendHello();  // the computer retries its microphone once the browser's recording works again
  } catch (_) {
    micError = "The browser couldn't record the microphone.";
    sendHello();
  }
}

function useCamera(track) {
  camera = track;
  try { track.contentHint = "motion"; } catch (_) { /* older browsers */ }
  const s = track.getSettings ? track.getSettings() : {};
  if (s.facingMode === "user" || s.facingMode === "environment") facing = s.facingMode;
  video.srcObject = new MediaStream([track]);
  // A camera that doesn't say which way it faces is a laptop's or a tablet's front one
  video.classList.toggle("mirror", s.facingMode !== "environment");
  video.play().catch(() => {});
  track.addEventListener("ended", onCameraEnded);
  torchOn = false;  // a camera opened again starts with its flash off
  if (!frontCamera()) screenLight.hidden = true;
  showLight();
  if (videoSender) videoSender.replaceTrack(track).then(applyLimits, () => {});
}

function useMic(track) {
  if (mic && mic !== track) mic.stop();
  mic = track;
  track.addEventListener("ended", () => {
    if (mic === track && running && document.visibilityState === "visible") openMic();
  });
  updateMicSender();
}

function stopCamera() {
  if (camera) camera.stop();
  camera = null;
}

function stopMedia() {
  stopCamera();
  if (mic) mic.stop();
  mic = null;
}

// The camera's own flash where it has one; a front camera without one gets the screen as its light
function hasTorch() {
  try {
    return !!(camera && camera.getCapabilities && camera.getCapabilities().torch);
  } catch (_) {
    return false;
  }
}

function frontCamera() {
  const s = camera && camera.getSettings ? camera.getSettings() : {};
  return s.facingMode !== "environment";
}

// What the camera is asked for, flash included: applyConstraints replaces everything asked before
function cameraConstraints() {
  return torchOn ? Object.assign(videoConstraints(), { advanced: [{ torch: true }] }) : videoConstraints();
}

function showLight() {
  lightBtn.hidden = !running || !camera || !(hasTorch() || frontCamera());
  lightBtn.classList.toggle("on", torchOn || !screenLight.hidden);
}

async function toggleLight() {
  if (hasTorch()) {
    torchOn = !torchOn;
    try {
      await camera.applyConstraints(cameraConstraints());
    } catch (_) {
      torchOn = !torchOn;  // refused: it stays as it was
    }
  } else {
    screenLight.hidden = !screenLight.hidden;
  }
  showLight();
}

function onCameraEnded() {
  // The camera was taken away (another app, or the page went to the background on iOS); get it back when visible.
  if (running && document.visibilityState === "visible") restartCamera();
}

async function restartCamera() {
  try {
    await openCamera();
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

// Flip only where there's another camera to flip to
async function showFlip() {
  let cameras = 2;
  try {
    cameras = (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.kind === "videoinput").length;
  } catch (_) { /* can't tell: offer it */ }
  flipBtn.hidden = !running || cameras < 2;
}

// ── Connecting ─────────────────────────────────────────────────────────────

function connect() {
  clearTimeout(retryTimer);
  const socket = new WebSocket(
    `wss://${location.host}/ws?token=${encodeURIComponent(token)}&id=${encodeURIComponent(browserId)}`);
  ws = socket;
  socket.onopen = () => {
    retries = 0;
    sendHello();
    show("Connecting the video…");
    negotiate();
  };
  socket.onmessage = (e) => {
    if (typeof e.data !== "string") return;
    let msg;
    try { msg = JSON.parse(e.data); } catch (_) { return; }
    if (msg.type === "config") applyConfig(msg);
    else if (msg.type === "answer") onAnswer(msg);
    else if (msg.type === "rtc_error") show(msg.message || "The computer couldn't set up the video connection.", "err");
  };
  socket.onclose = (e) => {
    if (ws !== socket) return;
    ws = null;
    closePeer();
    if (!running) return;
    const why = { 4000: "This camera was opened in another tab.",
                  4001: "All of your computer's virtual cameras are in use. Stop one there, then tap Start.",
                  4002: "This code was replaced on the computer. Scan the new one to stream again." }[e.code];
    if (why) {
      stop();
      show(why, "warn");
      startBtn.hidden = e.code === 4002;  // this code can't start anything again
      return;
    }
    retries += 1;
    show(retries > 3 ? "Can't reach the computer. If its code changed, scan it again." : "Reconnecting…",
         retries > 3 ? "err" : "warn");
    retryTimer = setTimeout(connect, Math.min(5000, 500 * retries));
  };
}

function sendHello() {
  send({ type: "hello", device: deviceName(), mic_error: micError });
}

function send(msg) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function applyConfig(msg) {
  const changed = msg.width !== config.width || msg.height !== config.height || msg.fps !== config.fps;
  const named = (msg.camera || "") !== (config.camera || "");
  config = { width: msg.width | 0 || 1280, height: msg.height | 0 || 720, fps: msg.fps | 0 || 30, audio: !!msg.audio,
             max_kbps: msg.max_kbps | 0 || 4000, camera: typeof msg.camera === "string" ? msg.camera : "" };
  if (named && pc && pc.connectionState === "connected") showConnected();
  updateMicSender();
  if (changed && camera) resize();
  else applyLimits();
}

// A new size or rate: the camera is asked for it, and opened again where the browser ignores that (Safari often does).
async function resize() {
  const track = camera;
  try {
    await track.applyConstraints(cameraConstraints());
  } catch (_) { /* reopened below */ }
  const s = track.getSettings ? track.getSettings() : {};
  const near = (a, b) => !a || Math.abs(a - b) < 64;
  const fits = near(Math.max(s.width, s.height), Math.max(config.width, config.height)) && near(s.frameRate, config.fps);
  if (track === camera && !fits && running) await restartCamera();
  applyLimits();
}

// ── WebRTC ─────────────────────────────────────────────────────────────────

async function negotiate() {
  closePeer();
  const socket = ws;
  const peer = new RTCPeerConnection({ iceServers: [], bundlePolicy: "max-bundle" });  // the local network only
  pc = peer;
  const vt = peer.addTransceiver(camera || "video", { direction: "sendonly" });
  videoSender = vt.sender;
  preferCodecs(vt);
  micSender = peer.addTransceiver("audio", { direction: "sendonly" }).sender;
  updateMicSender();
  peer.onconnectionstatechange = () => onPeerState(peer);
  try {
    await peer.setLocalDescription(await peer.createOffer());
    await iceGathered(peer);
  } catch (err) {
    if (pc === peer) show(`This browser couldn't set up the video connection (${err.name || err}).`, "err");
    return;
  }
  if (pc !== peer || ws !== socket) return;  // superseded while the addresses were gathered
  send({ type: "offer", sdp: peer.localDescription.sdp });
}

function iceGathered(peer) {
  if (peer.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ICE_WAIT_MS);
    peer.addEventListener("icegatheringstatechange", () => {
      if (peer.iceGatheringState !== "complete") return;
      clearTimeout(timer);
      resolve();
    });
  });
}

// H.264 first (the device's hardware encoder; the computer decodes Constrained Baseline and Baseline), then VP8
function preferCodecs(transceiver) {
  if (typeof transceiver.setCodecPreferences !== "function" || !RTCRtpSender.getCapabilities) return;
  const rank = (c) => {
    const type = c.mimeType.toLowerCase(), fmtp = c.sdpFmtpLine || "";
    if (type === "video/h264" && /packetization-mode=1/.test(fmtp)) {
      if (/profile-level-id=42e0/i.test(fmtp)) return 0;
      if (/profile-level-id=4200/i.test(fmtp)) return 1;
    }
    if (type === "video/vp8") return 2;
    return 3;  // rtx, red, ulpfec, other H.264 profiles and codecs the computer doesn't decode
  };
  try {
    const codecs = RTCRtpSender.getCapabilities("video").codecs;
    transceiver.setCodecPreferences([...codecs].sort((a, b) => rank(a) - rank(b)));
  } catch (_) { /* the browser's own order */ }
}

async function onAnswer(msg) {
  const peer = pc;
  if (!peer || typeof msg.sdp !== "string") return;
  try {
    await peer.setRemoteDescription({ type: "answer", sdp: msg.sdp });
  } catch (err) {
    if (pc === peer) show(`The video connection didn't start (${err.name || err}).`, "err");
    return;
  }
  applyLimits();
}

function onPeerState(peer) {
  if (pc !== peer) return;
  clearTimeout(downTimer);
  const state = peer.connectionState;
  if (state === "connected") {
    showConnected();
    clearInterval(statsTimer);
    statsTimer = setInterval(sendStats, STATS_SECONDS * 1000);
  } else if (state === "failed") {
    show("Reconnecting…", "warn");
    negotiate();
  } else if (state === "disconnected") {
    show("Reconnecting…", "warn");
    downTimer = setTimeout(() => { if (pc === peer && peer.connectionState !== "connected") negotiate(); },
                           DISCONNECTED_SECONDS * 1000);
  }
}

function closePeer() {
  clearTimeout(downTimer);
  clearInterval(statsTimer);
  const peer = pc;
  pc = videoSender = micSender = null;
  if (peer) {
    peer.onconnectionstatechange = null;
    try { peer.close(); } catch (_) { /* already closed */ }
  }
}

function updateMicSender() {
  if (!micSender) return;
  const track = config.audio && mic && mic.readyState === "live" ? mic : null;
  if (micSender.track !== track) micSender.replaceTrack(track).catch(() => {});
}

// The computer's limits for the encoder; a camera that delivers more than asked is scaled down here, not on the computer
async function applyLimits() {
  const sender = videoSender;
  if (!sender || !camera) return;
  try {
    const params = sender.getParameters();
    if (!params.encodings || !params.encodings.length) return;  // before the answer: applied again after it
    const s = camera.getSettings ? camera.getSettings() : {};
    const long = Math.max(s.width || 0, s.height || 0);
    const enc = params.encodings[0];
    enc.maxBitrate = config.max_kbps * 1000;
    enc.maxFramerate = config.fps;
    enc.scaleResolutionDownBy = long > 0 ? Math.max(1, long / Math.max(config.width, config.height)) : 1;
    await sender.setParameters(params);
  } catch (_) { /* some browsers refuse some of these; their defaults are fine */ }
}

async function sendStats() {
  const sender = videoSender;
  if (!sender) return;
  let out = null;
  try {
    (await sender.getStats()).forEach((r) => {
      if (r.type === "outbound-rtp" && r.kind !== "audio" && r.frameWidth) out = r;
    });
  } catch (_) { return; }
  if (!out || sender !== videoSender) return;
  const fps = Math.round(out.framesPerSecond || 0);
  const limit = out.qualityLimitationReason === "cpu" || out.qualityLimitationReason === "bandwidth" ? out.qualityLimitationReason : "";
  rateEl.textContent = `Sending ${out.frameWidth}×${out.frameHeight}` + (fps ? ` at ${fps} fps` : "");
  send({ type: "stats", width: out.frameWidth, height: out.frameHeight, fps, limit });
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
  if (!camera || camera.readyState === "ended") restartCamera();
  if (wantMic && (!mic || mic.readyState === "ended")) openMic();
  if (!ws) connect();
  else if (pc && pc.connectionState === "failed") negotiate();
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
  if (typeof RTCPeerConnection !== "function") {
    show("This browser can't send video. Try another browser, or update this one.", "err");
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
  stopBtn.hidden = darkBtn.hidden = false;
  showFlip();
  showLight();
  keepAwake();
  show("Connecting…");
  connect();
}

function stop() {
  running = false;
  mediaGen++;
  clearTimeout(retryTimer);
  const socket = ws;
  ws = null;
  closePeer();
  if (socket) socket.close(1000);
  stopMedia();
  wantMic = false;
  if (wakeLock) wakeLock.release().catch(() => {});
  wakeLock = null;
  video.srcObject = null;
  startBtn.hidden = false;
  startBtn.disabled = false;
  flipBtn.hidden = stopBtn.hidden = darkBtn.hidden = true;
  torchOn = false;
  screenLight.hidden = true;
  showLight();
  wake();
  if (blackVideo) blackVideo.pause();
  show("Stopped. Tap Start to use this camera again.");
}

startBtn.addEventListener("click", start);
lightBtn.addEventListener("click", () => { if (!justWoke()) toggleLight(); });
screenLight.addEventListener("click", () => {
  screenLight.hidden = true;
  showLight();
});
stopBtn.addEventListener("click", () => { if (!justWoke()) stop(); });
darkBtn.addEventListener("click", () => { if (!justWoke()) goDark(); });
flipBtn.addEventListener("click", async () => {
  if (justWoke()) return;
  facing = facing === "user" ? "environment" : "user";
  flipBtn.disabled = true;
  await restartCamera();
  flipBtn.disabled = false;
});

if (!token) show("This address is missing its code. Scan the code on the computer again.", "err");
