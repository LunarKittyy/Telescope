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
  const world = { sockets: [], contexts: [], nodes: [], encoders: [], supported: [], timers: [], timerId: 0, audio: true };
  const elements = {};
  const element = (id) => elements[id] || (elements[id] = {
    id, hidden: false, disabled: false, textContent: "", className: "", srcObject: null, videoWidth: 0, videoHeight: 0,
    listeners: {}, classList: { toggle() {} }, play: async () => {},
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

  const sandbox = {
    console, Blob, performance,
    location: { hash: "#token", host: "computer:8767" },
    navigator: { userAgent: "Mozilla/5.0 (X11; Linux x86_64) Chrome/120", mediaDevices: {
      getUserMedia: async () => stream(world.audio) } },
    crypto: require("crypto").webcrypto,
    localStorage: { getItem: () => null, setItem() {} },
    sessionStorage: { getItem: () => null, setItem() {} },
    document: {
      visibilityState: "visible", addEventListener() {}, getElementById: element,
      createElement: () => ({ width: 0, height: 0, getContext: () => ({ drawImage() {} }), toBlob() {} }),
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
