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
  const world = { sockets: [], contexts: [], nodes: [], encoders: [], supported: [], timers: [], timerId: 0, audio: true,
                  locks: [], fullscreen: null, fullscreenAsks: 0, now: 1000, videos: [],
                  videoPlays: true };
  const elements = {};
  const element = (id) => elements[id] || (elements[id] = {
    id, hidden: false, disabled: false, textContent: "", className: "", srcObject: null, videoWidth: 0, videoHeight: 0,
    listeners: {}, classes: new Set(), play: async () => {},
    get classList() { const c = this.classes; return { toggle: (name, on) => (on ?? !c.has(name)) ? c.add(name) : c.delete(name) }; },
    addEventListener(type, fn) { this.listeners[type] = fn; },
  });
  const track = (kind) => ({ kind, readyState: "live", stopped: false, stop() { this.stopped = true; },
                             addEventListener() {}, applyConstraints: async () => {} });
  const stream = (audio) => {
    const tracks = [track("video"), ...(audio ? [track("audio")] : [])];
    return { getTracks: () => tracks, getAudioTracks: () => tracks.filter((t) => t.kind === "audio"),
             getVideoTracks: () => tracks.filter((t) => t.kind === "video") };
  };

  class FakeContext {
    constructor() {
      this.state = "suspended";
      this.destination = {};
      this.module = deferred();
      this.audioWorklet = { addModule: () => this.module.promise };
      world.contexts.push(this);
    }
    createMediaStreamSource(media) { return { media, connect() {} }; }
    createGain() { return { gain: { value: 1 }, connect: (n) => n }; }
    async resume() { this.state = "running"; }
    async close() { this.state = "closed"; }
  }
  class FakeNode {
    constructor(ctx) { this.ctx = ctx; this.port = {}; world.nodes.push(this); }
    connect(n) { return n; }
  }
  class FakeSocket {
    constructor(url) { this.url = url; this.readyState = 0; this.sent = []; this.bufferedAmount = 0; world.sockets.push(this); }
    send(data) { this.sent.push(data); }
    close() { this.readyState = 3; }
    open() { this.readyState = 1; this.onopen(); }
    message(obj) { this.onmessage({ data: JSON.stringify(obj) }); }
  }
  FakeSocket.OPEN = 1;
  class FakeEncoder {
    constructor(init) { this.init = init; this.state = "unconfigured"; this.encodeQueueSize = 0; world.encoders.push(this); }
    configure(cfg) { this.state = "configured"; this.cfg = cfg; }
    encode() {}
    close() { this.state = "closed"; }
    static isConfigSupported() { const d = deferred(); world.supported.push(d); return d.promise; }
  }
  class FakeFrame { close() {} }
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

  const sandbox = {
    console, Blob, performance: { now: () => world.now },
    location: { hash: "#token", host: "computer:8767" },
    navigator: { userAgent: "Mozilla/5.0 (X11; Linux x86_64) Chrome/120", mediaDevices: {
      getUserMedia: async () => stream(world.audio) },
      wakeLock: { request: async () => new FakeLock() } },
    crypto: require("crypto").webcrypto,
    localStorage: { getItem: () => null, setItem() {} },
    sessionStorage: { getItem: () => null, setItem() {} },
    document: {
      visibilityState: "visible", addEventListener() {}, getElementById: element,
      documentElement: root, fullscreenEnabled: true,
      get fullscreenElement() { return world.fullscreen; },
      exitFullscreen: async () => { world.fullscreen = null; },
      createElement: (tag) => tag === "video" ? fakeVideo() : ({ width: 0, height: 0, toBlob() {}, captureStream: () => ({}),
                                                                getContext: () => ({ drawImage() {}, fillRect() {} }) }),
      body: { appendChild() {} },
    },
    AudioContext: FakeContext, AudioWorkletNode: FakeNode, WebSocket: FakeSocket,
    VideoEncoder: FakeEncoder, VideoFrame: FakeFrame,
    setTimeout: (fn) => { world.timers.push({ id: ++world.timerId, fn }); return world.timerId; },
    clearTimeout: (id) => { world.timers = world.timers.filter((t) => t.id !== id); },
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
  };
}

// A page that has started with the computer asking for H.264 and a camera frame ready: the frame timer's next run asks the capability check
async function pageAskingForH264() {
  const page = makePage();
  page.world.audio = false;
  await page.click("start");
  const socket = page.world.sockets[page.world.sockets.length - 1];
  socket.open();
  socket.message({ type: "config", width: 1280, height: 720, fps: 30, audio: false, h264: true });
  page.element("video").videoWidth = 1280;
  page.element("video").videoHeight = 720;
  return page;
}

// Start, Flip (its mic setup stays pending), Stop, Start (its setup finishes): contexts 0, 1 (the Flip's) and 2
async function flipStopStart() {
  const page = makePage();
  const first = page.click("start");
  await page.flush();
  page.world.contexts[0].module.resolve();
  await first;
  page.element("flip").listeners.click();
  await page.flush();
  assert.strictEqual(page.world.contexts.length, 2);
  await page.click("stop");
  const second = page.click("start");
  await page.flush();
  assert.strictEqual(page.world.contexts.length, 3);
  page.world.contexts[2].module.resolve();
  await second;
  assert.strictEqual(page.world.nodes.length, 2);
  return page;
}

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

const nodeContexts = (page) => page.world.nodes.map((n) => page.world.contexts.indexOf(n.ctx));

const scenarios = {
  // Without a Stop the check builds the encoder (shows the harness drives the page properly)
  async the_capability_check_builds_the_encoder() {
    const page = await pageAskingForH264();
    page.runTimers();
    page.world.supported[0].resolve({ supported: true });
    await page.flush();
    assert.strictEqual(page.world.encoders.length, 1);
    assert.strictEqual(page.world.encoders[0].state, "configured");
    assert.strictEqual(page.value("codecNow"), "h264");
  },

  // Stop while the capability check is pending must not leave an encoder behind
  async stop_during_the_capability_check() {
    const page = await pageAskingForH264();
    page.runTimers();
    assert.strictEqual(page.world.supported.length, 1);
    await page.click("stop");
    page.world.supported[0].resolve({ supported: true });
    await page.flush();
    assert.strictEqual(page.world.encoders.length, 0, "an encoder was created after Stop");
    assert.strictEqual(page.value("codecNow"), "", "the codec was reported after Stop");
    assert.strictEqual(page.value("configuring"), false);
  },

  // A check that answers "no encoder" after Stop must not change the fallback state either
  async stop_during_a_failed_capability_check() {
    const page = await pageAskingForH264();
    page.runTimers();
    await page.click("stop");
    page.world.supported[0].resolve({ supported: false });
    await page.flush();
    page.world.supported[1].resolve({ supported: false });  // the second codec is tried too
    await page.flush();
    assert.strictEqual(page.value("h264Off"), "", "the fallback was recorded after Stop");
    assert.strictEqual(page.value("longCap"), Infinity, "the size cap changed after Stop");
    assert.strictEqual(page.value("codecNow"), "");
  },

  // A check that answers after Stop and Start again is ignored; the new run sets up its own encoder
  async a_stale_capability_check_does_not_serve_the_next_run() {
    const page = await pageAskingForH264();
    page.runTimers();
    await page.click("stop");
    await page.click("start");
    const socket = page.world.sockets[1];
    socket.open();
    socket.message({ type: "config", width: 1280, height: 720, fps: 30, audio: false, h264: true });
    page.world.supported[0].resolve({ supported: true });  // the old run's answer
    await page.flush();
    assert.strictEqual(page.world.encoders.length, 0, "the old run's answer built an encoder");
    page.element("video").videoWidth = 1280;
    page.element("video").videoHeight = 720;
    page.runTimers();  // the new run asks for itself
    assert.strictEqual(page.world.supported.length, 2);
    page.world.supported[1].resolve({ supported: true });
    await page.flush();
    assert.strictEqual(page.world.encoders.length, 1);
    assert.strictEqual(page.world.encoders[0].state, "configured");
    assert.strictEqual(page.value("codecNow"), "h264");
  },

  // Flip, then Stop and Start while the Flip's mic setup is pending: only the new run records the mic
  async a_mic_setup_from_before_stop_adds_no_second_sender() {
    const page = await flipStopStart();
    page.world.contexts[1].module.resolve();  // the Flip's setup finishes last
    await page.flush();
    assert.deepStrictEqual(nodeContexts(page), [0, 2]);
    assert.strictEqual(page.value("micError"), "");
  },

  // The same, with the old setup failing because its context was closed
  async a_failed_mic_setup_from_before_stop_leaves_the_mic_error_alone() {
    const page = await flipStopStart();
    page.world.contexts[1].module.reject(new Error("closed"));
    await page.flush();
    assert.deepStrictEqual(nodeContexts(page), [0, 2]);
    assert.strictEqual(page.value("micError"), "");
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
