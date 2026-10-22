// Runs the Browser camera page (telescope/web/app.js) against stand-ins for the browser, with promise timing under the test's control.
// Usage: node page_harness.js --list | node page_harness.js <scenario>
"use strict";
const fs = require("fs");
const path = require("path");
const vm = require("vm");
const assert = require("assert");

const APP = path.join(__dirname, "..", "..", "telescope", "web", "app.js");

function deferred() {
  const d = {};
  d.promise = new Promise((resolve, reject) => { d.resolve = resolve; d.reject = reject; });
  return d;
}

const flush = async () => { for (let i = 0; i < 5; i++) await new Promise((r) => setImmediate(r)); };

function makePage() {
  const world = { sockets: [], peers: [], gum: [], timers: [], timerId: 0, audio: true, locks: [], fullscreen: null,
                  fullscreenAsks: 0, now: 1000, videos: [], videoPlays: true, iceComplete: true,
                  cameraSettings: { width: 1280, height: 720, frameRate: 30, facingMode: "user" },
                  devices: [{ kind: "videoinput" }, { kind: "videoinput" }, { kind: "audioinput" }] };
  const elements = {};
  const element = (id) => elements[id] || (elements[id] = {
    id, hidden: false, disabled: false, textContent: "", className: "", srcObject: null, videoWidth: 0, videoHeight: 0,
    listeners: {}, classes: new Set(), play: async () => {},
    get classList() { const c = this.classes; return { toggle: (name, on) => (on ?? !c.has(name)) ? c.add(name) : c.delete(name) }; },
    addEventListener(type, fn) { this.listeners[type] = fn; },
  });
  element("screen-light").hidden = true;  // hidden in index.html
  const track = (kind, settings) => ({
    kind, readyState: "live", stopped: false, listeners: {}, settings: { ...settings },
    stop() { this.stopped = true; this.readyState = "ended"; },
    end() { this.readyState = "ended"; for (const fn of this.listeners.ended || []) fn(); },
    addEventListener(type, fn) { (this.listeners[type] = this.listeners[type] || []).push(fn); },
    applyConstraints: async (c) => { world.applied = c; },
    getSettings() { return this.settings; },
    getCapabilities() { return kind === "video" ? world.capabilities || {} : {}; },
  });
  const getUserMedia = async (constraints) => {
    world.gum.push(constraints);
    if (world.gumFails) throw Object.assign(new Error("no"), { name: world.gumFails });
    const tracks = [];
    if (constraints.video) tracks.push(track("video", world.cameraSettings));
    if (constraints.audio && world.audio) tracks.push(track("audio", {}));
    return { getTracks: () => tracks, getAudioTracks: () => tracks.filter((t) => t.kind === "audio"),
             getVideoTracks: () => tracks.filter((t) => t.kind === "video") };
  };

  class FakeSocket {
    constructor(url) { this.url = url; this.readyState = 0; this.sent = []; world.sockets.push(this); }
    send(data) { this.sent.push(JSON.parse(data)); }
    close() { this.readyState = 3; }
    open() { this.readyState = 1; this.onopen(); }
    message(obj) { this.onmessage({ data: JSON.stringify(obj) }); }
    closed(code) { this.readyState = 3; this.onclose({ code }); }
    of(type) { return this.sent.filter((m) => m.type === type); }
  }
  FakeSocket.OPEN = 1;
  class FakeSender {
    constructor(peer, track) { this.peer = peer; this.track = track; this.replaced = []; this.set = null; this.enc = {}; }
    async replaceTrack(t) { this.track = t; this.replaced.push(t); }
    getParameters() { return { encodings: this.peer.remoteDescription ? [this.enc] : [] }; }
    async setParameters(p) { this.set = p; }
    async getStats() { return new Map(Object.entries(world.stats || {})); }
  }
  class FakePeer {
    constructor(cfg) {
      this.cfg = cfg; this.connectionState = "new"; this.iceGatheringState = world.iceComplete ? "complete" : "gathering";
      this.transceivers = []; this.listeners = {}; this.closed = false; this.remoteDescription = null;
      world.peers.push(this);
    }
    addTransceiver(what, init) {
      const t = { kind: typeof what === "string" ? what : what.kind, init,
                  sender: new FakeSender(this, typeof what === "string" ? null : what),
                  setCodecPreferences(list) { this.prefs = list; } };
      this.transceivers.push(t);
      return t;
    }
    async createOffer() { return { type: "offer", sdp: `offer ${world.peers.indexOf(this)}` }; }
    async setLocalDescription(d) { this.localDescription = d; }
    async setRemoteDescription(d) { this.remoteDescription = d; }
    addEventListener(type, fn) { this.listeners[type] = fn; }
    gathered() { this.iceGatheringState = "complete"; this.listeners.icegatheringstatechange(); }
    state(s) { this.connectionState = s; if (this.onconnectionstatechange) this.onconnectionstatechange(); }
    close() { this.closed = true; this.connectionState = "closed"; }
  }
  const FakeRtpSender = { getCapabilities: () => ({ codecs: [
    { mimeType: "video/VP9" }, { mimeType: "video/H264", sdpFmtpLine: "packetization-mode=1;profile-level-id=640c1f" },
    { mimeType: "video/rtx" }, { mimeType: "video/VP8" },
    { mimeType: "video/H264", sdpFmtpLine: "packetization-mode=0;profile-level-id=42e01f" },
    { mimeType: "video/H264", sdpFmtpLine: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f" },
  ] }) };
  class FakeLock {
    constructor() { this.released = false; this.listeners = {}; world.locks.push(this); }
    addEventListener(type, fn) { this.listeners[type] = fn; }
    async release() { this.drop(); }
    drop() { if (this.released) return; this.released = true; if (this.listeners.release) this.listeners.release(); }
  }
  // A video that can only go full screen in the system player, the way iPhone Safari does it
  const fakeVideo = () => {
    const v = { listeners: {}, paused: true, plays: 0, readyState: 4, webkitDisplayingFullscreen: false, setAttribute() {},
                addEventListener(type, fn) { this.listeners[type] = fn; },
                play: async () => {
                  v.plays += 1;
                  if (!world.videoPlays || !v.paused) return;
                  v.paused = false;
                  if (v.listeners.playing) v.listeners.playing();
                },
                pause() { if (this.paused) return; this.paused = true; if (this.listeners.pause) this.listeners.pause(); },
                webkitEnterFullscreen() { this.webkitDisplayingFullscreen = true; },
                webkitExitFullscreen() { this.close(); },
                userPause() { this.pause(); },
                close() { if (!this.webkitDisplayingFullscreen) return; this.webkitDisplayingFullscreen = false; this.listeners.webkitendfullscreen(); } };
    world.videos.push(v);
    return v;
  };
  const root = { requestFullscreen: async () => { world.fullscreenAsks += 1; world.fullscreen = root; } };
  const timer = (fn) => { world.timers.push({ id: ++world.timerId, fn }); return world.timerId; };
  const clear = (id) => { world.timers = world.timers.filter((t) => t.id !== id); };

  const sandbox = {
    console, performance: { now: () => world.now },
    location: { hash: "#token", host: "computer:8767" },
    navigator: { userAgent: "Mozilla/5.0 (X11; Linux x86_64) Chrome/120", mediaDevices: {
      getUserMedia, enumerateDevices: async () => world.devices },
      wakeLock: { request: async () => new FakeLock() } },
    crypto: require("crypto").webcrypto,
    localStorage: { getItem: () => null, setItem() {} },
    sessionStorage: { getItem: () => null, setItem() {} },
    document: {
      visibilityState: "visible", addEventListener() {}, getElementById: element,
      documentElement: root, fullscreenEnabled: true,
      get fullscreenElement() { return world.fullscreen; },
      exitFullscreen: async () => { world.fullscreen = null; },
      createElement: (tag) => tag === "video" ? fakeVideo() : ({ width: 0, height: 0, captureStream: () => ({}),
                                                                getContext: () => ({ fillRect() {} }) }),
      body: { appendChild() {} },
    },
    WebSocket: FakeSocket, RTCPeerConnection: FakePeer, RTCRtpSender: FakeRtpSender,
    MediaStream: class { constructor(tracks) { this.tracks = tracks; } },
    setTimeout: timer, clearTimeout: clear, setInterval: timer, clearInterval: clear,
  };
  sandbox.window = sandbox;
  sandbox.isSecureContext = true;
  const context = vm.createContext(sandbox);
  vm.runInContext(fs.readFileSync(APP, "utf8"), context, { filename: "app.js" });
  return {
    world, element,
    value: (expr) => vm.runInContext(expr, context),
    click: (id) => element(id).listeners.click(),
    runTimer(id) { const t = world.timers.find((x) => x.id === id); world.timers = world.timers.filter((x) => x !== t); t.fn(); },
    runTimers() { const due = world.timers; world.timers = []; for (const t of due) t.fn(); },
    flush,
    socket: () => world.sockets[world.sockets.length - 1],
    peer: () => world.peers[world.peers.length - 1],
  };
}

// A started page connected to the computer, its offer answered: what the next WebRTC scenarios start from
async function connectedPage(setUp) {
  const page = makePage();
  if (setUp) setUp(page.world);
  await page.click("start");
  const socket = page.socket();
  socket.open();
  await page.flush();
  socket.message({ type: "config", width: 1280, height: 720, fps: 30, audio: false, max_kbps: 4000 });
  socket.message({ type: "answer", sdp: "answer" });
  await page.flush();
  page.peer().state("connected");
  return page;
}

const videoSender = (peer) => peer.transceivers.find((t) => t.kind === "video").sender;
const micSender = (peer) => peer.transceivers.find((t) => t.kind === "audio").sender;

// A started page that has gone to the black screen
async function darkPage(fullscreen = true) {
  const page = makePage();
  page.world.audio = false;
  page.value("document").fullscreenEnabled = fullscreen;
  await page.click("start");
  page.click("dark");
  return page;
}

// The same on an iPhone: no page full screen, but videos can go full screen in the system player
async function iphoneDarkPage() {
  const page = makePage();
  page.world.audio = false;
  page.value("document").fullscreenEnabled = false;
  page.element("video").webkitEnterFullscreen = () => {};
  await page.click("start");
  page.click("dark");
  return page;
}

const tap = (page) => page.element("black").listeners.click();
const faded = (page) => page.element("wake-hint").classes.has("faded");


const scenarios = {
  // Connecting says hello, offers H.264 first with every address, and sets the computer's limits once answered
  async the_page_offers_and_applies_the_answer() {
    const page = await connectedPage();
    const socket = page.socket(), peer = page.peer();
    assert.deepStrictEqual(socket.sent.map((m) => m.type), ["hello", "offer"]);
    assert.strictEqual(socket.of("offer")[0].sdp, "offer 0");
    assert.strictEqual(peer.cfg.iceServers.length, 0, "a server outside the network was asked for addresses");
    const prefs = peer.transceivers[0].prefs.map((c) => `${c.mimeType} ${c.sdpFmtpLine || ""}`);
    assert.ok(prefs[0].includes("packetization-mode=1;profile-level-id=42e01f"), prefs[0]);
    assert.strictEqual(prefs[1], "video/VP8 ");
    assert.strictEqual(peer.remoteDescription.sdp, "answer");
    const enc = videoSender(peer).set.encodings[0];
    assert.strictEqual(enc.maxBitrate, 4_000_000);
    assert.strictEqual(enc.maxFramerate, 30);
    assert.strictEqual(enc.scaleResolutionDownBy, 1);
    assert.strictEqual(page.element("status").className, "");
    assert.ok(page.element("status").textContent.startsWith("Connected"));
  },

  // An offer whose addresses are still being gathered when a newer one starts is never sent
  async a_superseded_offer_is_not_sent() {
    const page = makePage();
    page.world.iceComplete = false;
    await page.click("start");
    const socket = page.socket();
    socket.open();
    await page.flush();
    const first = page.peer();
    first.state("failed");  // starts over before the first one's addresses are in
    await page.flush();
    first.gathered();
    await page.flush();
    assert.strictEqual(socket.of("offer").length, 0, "the old connection's offer went out");
    page.peer().gathered();
    await page.flush();
    assert.strictEqual(socket.of("offer").length, 1);
    assert.ok(first.closed);
  },

  // A connection that fails is set up again over the same socket; a disconnected one gets a moment first
  async a_failed_connection_offers_again() {
    const page = await connectedPage();
    const socket = page.socket();
    page.peer().state("failed");
    await page.flush();
    assert.strictEqual(page.world.peers.length, 2);
    assert.strictEqual(socket.of("offer").length, 2);
    page.peer().state("disconnected");
    await page.flush();
    assert.strictEqual(page.world.peers.length, 2, "a short disconnect started over at once");
    page.runTimer(page.value("downTimer"));
    await page.flush();
    assert.strictEqual(page.world.peers.length, 3);
  },

  // Flip opens the other camera alone: the mic keeps going and the new camera goes out on the same connection
  async flip_keeps_the_mic() {
    const page = await connectedPage();
    const mic = page.value("mic");
    assert.ok(mic && !mic.stopped);
    page.world.cameraSettings = { width: 1280, height: 720, frameRate: 30, facingMode: "environment" };
    await page.click("flip");
    await page.flush();
    const last = page.world.gum[page.world.gum.length - 1];
    assert.strictEqual(last.audio, false, "Flip asked for the mic again");
    assert.ok(!mic.stopped, "Flip stopped the mic");
    assert.strictEqual(page.value("mic"), mic);
    const sender = videoSender(page.peer());
    assert.strictEqual(sender.track, page.value("camera"));
    assert.strictEqual(page.world.peers.length, 1, "Flip set the connection up again");
    assert.ok(!page.element("video").classes.has("mirror"), "the back camera is mirrored");
  },

  // The preview is mirrored by the camera's own facing; one that doesn't say (a laptop's) counts as a front one
  async the_mirror_follows_the_camera_not_the_request() {
    const page = await connectedPage((w) => { w.cameraSettings = { width: 1280, height: 720 }; });
    assert.ok(page.element("video").classes.has("mirror"));
    page.world.cameraSettings = { width: 1280, height: 720 };
    await page.click("flip");  // asks for the back camera; this device only has the one
    await page.flush();
    assert.ok(page.element("video").classes.has("mirror"), "the same front camera lost its mirror");
  },

  // The mic only goes out while the computer asks for it
  async the_mic_follows_the_computer() {
    const page = await connectedPage();
    const sender = micSender(page.peer());
    assert.strictEqual(sender.track, null);
    page.socket().message({ type: "config", width: 1280, height: 720, fps: 30, audio: true, max_kbps: 4000 });
    assert.strictEqual(sender.track, page.value("mic"));
    page.socket().message({ type: "config", width: 1280, height: 720, fps: 30, audio: false, max_kbps: 4000 });
    assert.strictEqual(sender.track, null);
  },

  // A mic the browser took away is opened again on its own once the page is back
  async a_lost_mic_comes_back() {
    const page = await connectedPage();
    page.socket().message({ type: "config", width: 1280, height: 720, fps: 30, audio: true, max_kbps: 4000 });
    const mic = page.value("mic");
    mic.end();
    await page.flush();
    const again = page.value("mic");
    assert.notStrictEqual(again, mic);
    assert.strictEqual(micSender(page.peer()).track, again);
    assert.deepStrictEqual(page.world.gum[page.world.gum.length - 1].video, false);
  },

  // Without the mic the camera still starts, and the computer hears why
  async a_refused_mic_leaves_the_camera_working() {
    const page = makePage();
    const gum = page.value("navigator").mediaDevices.getUserMedia;
    page.value("navigator").mediaDevices.getUserMedia = async (c) => {
      if (c.audio) throw Object.assign(new Error("no"), { name: "NotAllowedError" });
      return gum(c);
    };
    await page.click("start");
    page.socket().open();
    await page.flush();
    assert.strictEqual(page.value("running"), true);
    assert.ok(page.socket().of("hello")[0].mic_error.includes("microphone"));
  },

  // A new size the camera ignores (Safari) opens it again; a camera bigger than asked is scaled down when sending
  async a_size_the_camera_ignores_reopens_it() {
    const page = await connectedPage();
    const asked = page.world.gum.length;
    page.socket().message({ type: "config", width: 1920, height: 1080, fps: 30, audio: false, max_kbps: 9000 });
    await page.flush();
    assert.strictEqual(page.world.applied.width.ideal, 1920);
    assert.strictEqual(page.world.gum.length, asked + 1, "a camera stuck at 720p wasn't opened again");
    page.socket().message({ type: "config", width: 854, height: 480, fps: 15, audio: false, max_kbps: 1500 });
    page.world.cameraSettings = { width: 1280, height: 720, frameRate: 15, facingMode: "user" };
    await page.flush();
    const enc = videoSender(page.peer()).set.encodings[0];
    assert.strictEqual(enc.maxBitrate, 1_500_000);
    assert.strictEqual(enc.maxFramerate, 15);
  },

  // New link on the computer stops the page for good instead of retrying a dead code
  async a_new_link_stops_the_page() {
    const page = await connectedPage();
    page.socket().closed(4002);
    assert.strictEqual(page.value("running"), false);
    assert.ok(page.element("status").textContent.includes("replaced"));
    assert.strictEqual(page.element("start").hidden, true, "Start was offered for a code that no longer works");
    assert.strictEqual(page.world.timers.filter((t) => String(t.fn).includes("connect")).length, 0);
    assert.strictEqual(page.world.sockets.length, 1);
    assert.ok(page.peer().closed);
  },

  // Any other drop reconnects and offers again
  async a_dropped_socket_reconnects() {
    const page = await connectedPage();
    page.socket().closed(1006);
    assert.strictEqual(page.value("running"), true);
    assert.ok(page.peer().closed);
    page.runTimer(page.value("retryTimer"));
    page.socket().open();
    await page.flush();
    assert.strictEqual(page.world.sockets.length, 2);
    assert.strictEqual(page.socket().of("offer").length, 1);
  },

  // The page shows and reports what really goes out
  async stats_report_what_is_sent() {
    const page = await connectedPage();
    page.world.stats = { a: { type: "outbound-rtp", kind: "video", frameWidth: 1280, frameHeight: 720,
                              framesPerSecond: 29.7, qualityLimitationReason: "cpu" } };
    page.runTimer(page.value("statsTimer"));
    await page.flush();
    assert.deepStrictEqual(page.socket().of("stats")[0], { type: "stats", width: 1280, height: 720, fps: 30, limit: "cpu" });
    assert.strictEqual(page.element("rate").textContent, "Sending 1280×720 at 30 fps");
  },

  // Flip only shows where there's another camera
  async one_camera_hides_flip() {
    const page = makePage();
    page.world.devices = [{ kind: "videoinput" }, { kind: "audioinput" }];
    await page.click("start");
    await page.flush();
    assert.strictEqual(page.element("flip").hidden, true);
    const two = makePage();
    await two.click("start");
    await two.flush();
    assert.strictEqual(two.element("flip").hidden, false);
  },

  // A front camera without a flash gets the screen as its light; a back one without a flash gets no Light at all
  async a_front_camera_lights_the_screen() {
    const page = await connectedPage();
    assert.strictEqual(page.element("light").hidden, false);
    await page.click("light");
    assert.strictEqual(page.element("screen-light").hidden, false);
    assert.ok(page.element("light").classes.has("on"));
    page.element("screen-light").listeners.click();
    assert.strictEqual(page.element("screen-light").hidden, true);
    assert.ok(!page.element("light").classes.has("on"));
    await page.click("light");
    page.world.cameraSettings = { width: 1280, height: 720, frameRate: 30, facingMode: "environment" };
    await page.click("flip");
    await page.flush();
    assert.strictEqual(page.element("screen-light").hidden, true, "the screen stayed white for the back camera");
    assert.strictEqual(page.element("light").hidden, true);
  },

  // A camera with a flash turns it on, keeping the size it was asked for, and keeps it on through a new size
  async a_flash_stays_on_through_a_new_size() {
    const page = await connectedPage((w) => {
      w.capabilities = { torch: true };
      w.cameraSettings = { width: 1280, height: 720, frameRate: 30, facingMode: "environment" };
    });
    await page.click("light");
    assert.strictEqual(page.world.applied.advanced[0].torch, true);
    assert.strictEqual(page.world.applied.width.ideal, 1280, "turning the flash on dropped the size");
    assert.strictEqual(page.element("screen-light").hidden, true);
    page.socket().message({ type: "config", width: 854, height: 480, fps: 30, audio: false, max_kbps: 1800 });
    await page.flush();
    assert.strictEqual(page.world.applied.width.ideal, 854);
    assert.strictEqual(page.world.applied.advanced[0].torch, true, "a new size turned the flash off");
    await page.click("stop");
    assert.strictEqual(page.value("torchOn"), false);
    assert.strictEqual(page.element("light").hidden, true);
  },

  // Stop while the camera is still opening leaves nothing running once it opens
  async stop_while_the_camera_opens() {
    const page = makePage();
    let release;
    const gum = page.value("navigator").mediaDevices.getUserMedia;
    page.value("navigator").mediaDevices.getUserMedia = (c) => new Promise((resolve) => { release = () => resolve(gum(c)); });
    const started = page.click("start");
    await page.flush();
    page.value("stop()");
    release();
    await started;
    await page.flush();
    assert.strictEqual(page.value("camera"), null);
    assert.strictEqual(page.world.sockets.length, 0);
  },

  // One tap shows the hint and waits; a second one in time wakes the screen and leaves full screen
  async a_double_tap_wakes_the_black_screen() {
    const page = await darkPage();
    assert.strictEqual(page.element("black").hidden, false);
    assert.strictEqual(page.world.fullscreenAsks, 1);
    page.runTimer(page.value("hintTimer"));
    assert.ok(faded(page), "the hint didn't fade");
    tap(page);
    assert.strictEqual(page.element("black").hidden, false, "one tap woke it");
    assert.ok(!faded(page), "a tap didn't bring the hint back");
    tap(page);
    assert.strictEqual(page.element("black").hidden, true);
    assert.strictEqual(page.world.fullscreen, null, "full screen was left on");
  },

  // A first tap whose second never comes fades out, so the next single tap doesn't wake it
  async a_lone_tap_times_out() {
    const page = await darkPage();
    tap(page);
    page.runTimer(page.value("hintTimer"));
    assert.ok(faded(page));
    tap(page);
    assert.strictEqual(page.element("black").hidden, false, "two taps far apart woke it");
  },

  // The tap that turned the screen black doesn't count as the first of two
  async the_button_tap_is_not_half_a_double_tap() {
    const page = await darkPage();
    tap(page);
    assert.strictEqual(page.element("black").hidden, false);
  },

  // Where a page can't go full screen (iPhone) the black screen still works
  async black_screen_without_full_screen() {
    const page = await darkPage(false);
    assert.strictEqual(page.world.fullscreenAsks, 0);
    assert.strictEqual(page.element("black").hidden, false);
    tap(page);
    tap(page);
    assert.strictEqual(page.element("black").hidden, true);
  },

  // On an iPhone the black screen shows a black video in the system player, and closing the player wakes the page
  async iphone_black_screen_uses_the_video_player() {
    const page = await iphoneDarkPage();
    assert.strictEqual(page.world.videos.length, 1);
    const v = page.world.videos[0];
    assert.ok(!v.paused, "the black video wasn't playing before the tap");
    assert.ok(v.webkitDisplayingFullscreen, "the black video didn't go full screen");
    assert.strictEqual(page.world.fullscreenAsks, 0);
    v.close();
    assert.strictEqual(page.element("black").hidden, true, "closing the player didn't wake the page");
    page.click("dark");
    assert.ok(v.webkitDisplayingFullscreen, "a second Black screen didn't use the player again");
    assert.strictEqual(page.world.videos.length, 1, "a second black video was made");
  },

  // A camera preview that stops moving behind the player closes it for good; the page stays black
  async a_stalled_camera_closes_the_iphone_player() {
    const page = await iphoneDarkPage();
    const v = page.world.videos[0];
    for (let i = 0; i < 3; i++) page.runTimer(page.value("playerTimer"));
    assert.strictEqual(v.webkitDisplayingFullscreen, false, "the player stayed up over a frozen camera");
    assert.strictEqual(page.element("black").hidden, false, "closing the player for a stall woke the page");
    tap(page);
    tap(page);
    page.world.now += 1000;
    page.click("dark");
    assert.strictEqual(v.webkitDisplayingFullscreen, false, "the player was used again after a stall");
    assert.strictEqual(page.element("black").hidden, false);
  },

  // A moving camera keeps the player up
  async a_moving_camera_keeps_the_iphone_player() {
    const page = await iphoneDarkPage();
    for (let i = 0; i < 5; i++) {
      page.element("video").currentTime = i + 1;
      page.runTimer(page.value("playerTimer"));
    }
    assert.ok(page.world.videos[0].webkitDisplayingFullscreen);
  },

  // A black video that never starts playing is left alone; the black page works as usual
  async a_black_video_that_does_not_play_is_not_used() {
    const page = makePage();
    page.world.audio = false;
    page.world.videoPlays = false;
    page.value("document").fullscreenEnabled = false;
    page.element("video").webkitEnterFullscreen = () => {};
    await page.click("start");
    page.click("dark");
    assert.strictEqual(page.world.videos[0].webkitDisplayingFullscreen, false);
    assert.strictEqual(page.element("black").hidden, false);
    tap(page);
    tap(page);
    assert.strictEqual(page.element("black").hidden, true);
  },

  // If making the black video throws, Start still works and Black screen falls back to the black page
  async a_failing_black_video_leaves_start_working() {
    const page = makePage();
    page.world.audio = false;
    page.value("document").fullscreenEnabled = false;
    page.element("video").webkitEnterFullscreen = () => {};
    page.value("document").createElement = () => { throw new Error("no"); };
    await page.click("start");
    assert.strictEqual(page.value("running"), true);
    assert.strictEqual(page.element("dark").hidden, false);
    page.click("dark");
    assert.strictEqual(page.element("black").hidden, false);
  },

  // iOS pauses the video when its player closes; the next Black screen plays it and uses the player again
  async the_player_works_again_after_ios_paused_the_video() {
    const page = await iphoneDarkPage();
    const v = page.world.videos[0];
    v.close();
    v.pause();
    page.world.now += 1000;
    page.click("dark");
    assert.ok(v.webkitDisplayingFullscreen, "a paused black video kept the player from being used again");
    await page.flush();
    assert.strictEqual(v.paused, false);
  },

  // Pausing with the player's own controls resumes, so the controls fade away again
  async pausing_in_the_player_resumes() {
    const page = await iphoneDarkPage();
    const v = page.world.videos[0];
    const before = v.plays;
    v.userPause();
    await page.flush();
    assert.strictEqual(v.plays, before + 1, "a pause in the player wasn't resumed");
    assert.strictEqual(v.paused, false);
  },

  // Stop while the player is up closes it
  async stop_closes_the_iphone_player() {
    const page = await iphoneDarkPage();
    await page.click("stop");
    assert.strictEqual(page.world.videos[0].webkitDisplayingFullscreen, false);
    assert.strictEqual(page.element("black").hidden, true);
  },

  // Where the page itself can go full screen there's no black video at all
  async no_black_video_where_the_page_can_go_full_screen() {
    const page = makePage();
    page.world.audio = false;
    page.element("video").webkitEnterFullscreen = () => {};
    await page.click("start");
    page.click("dark");
    assert.strictEqual(page.world.videos.length, 0);
    assert.strictEqual(page.world.fullscreenAsks, 1);
  },

  // Stop takes the black screen and the button away
  async stop_wakes_the_black_screen() {
    const page = await darkPage();
    await page.click("stop");
    assert.strictEqual(page.element("black").hidden, true);
    assert.strictEqual(page.element("dark").hidden, true);
    assert.strictEqual(page.world.fullscreen, null);
  },

  // A wake lock the browser drops while the page is showing is asked for again, but not after Stop
  async a_dropped_wake_lock_is_asked_for_again() {
    const page = makePage();
    page.world.audio = false;
    await page.click("start");
    await page.flush();
    assert.strictEqual(page.world.locks.length, 1);
    page.world.now += 6000;
    page.world.locks[0].drop();
    await page.flush();
    assert.strictEqual(page.world.locks.length, 2, "the lock wasn't asked for again");
    await page.click("stop");
    await page.flush();
    assert.ok(page.world.locks[1].released);
    assert.strictEqual(page.world.locks.length, 2, "Stop's own release asked for a new lock");
  },

  // A lock taken back right after it was granted isn't asked for again, or a browser doing that would get asked in a loop
  async a_lock_dropped_straight_away_is_not_asked_for_again() {
    const page = makePage();
    page.world.audio = false;
    await page.click("start");
    await page.flush();
    page.world.now += 100;
    page.world.locks[0].drop();
    await page.flush();
    assert.strictEqual(page.world.locks.length, 1);
  },

  // A third quick tap after the double tap lands on whatever button is under it, and is ignored for a moment
  async a_third_tap_after_waking_does_not_press_stop() {
    const page = await darkPage();
    tap(page);
    tap(page);
    page.world.now += 100;
    await page.click("stop");
    assert.strictEqual(page.value("running"), true, "the third tap stopped the stream");
    page.world.now += 1000;
    await page.click("stop");
    assert.strictEqual(page.value("running"), false);
  },
};

(async () => {
  const name = process.argv[2];
  if (name === "--list") {
    console.log(Object.keys(scenarios).join("\n"));
    return;
  }
  if (!scenarios[name]) throw new Error(`no scenario called ${name}`);
  await scenarios[name]();
})().then(() => process.exit(0), (err) => { console.error(err && err.stack || err); process.exit(1); });
