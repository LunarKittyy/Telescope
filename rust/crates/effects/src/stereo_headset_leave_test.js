"use strict";
// Runs the headset page's script against a stand-in headset and checks which controller presses take you out of VR.
const assert = require("assert");
const fs = require("fs");
const vm = require("vm");

const element = () => ({ getContext: () => ({}), addEventListener(name, handler) { this[name] = handler; }, textContent: "", width: 0, height: 0, disabled: false });
const enter = element();
const gl = new Proxy({}, { get: (_, name) => (name === "getAttribLocation" ? () => 0 : () => ({})) });
const pad = (...down) => ({ gamepad: { buttons: Array.from({ length: 6 }, (_, i) => ({ pressed: down.includes(i) })) } });
let ends = 0;
let frames = [];
const listeners = {};
const spaceListeners = {};
// a head at (1, 1.6, 2) turned a quarter to the left (yaw 90 degrees)
let head = [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 1.6, 2, 1];
const fake = {
  inputSources: [],
  renderState: {},
  updateRenderState(state) { this.renderState = state; },
  requestReferenceSpace: async () => ({ addEventListener: (name, f) => { spaceListeners[name] = f; } }),
  requestAnimationFrame: (f) => frames.push(f),
  addEventListener: (name, f) => { listeners[name] = f; },
  end: async () => { ends++; listeners.end(); },
};
const context = vm.createContext({
  location: { hash: "#token", host: "host" },
  document: { getElementById: (id) => (id === "enter" ? enter : element()), createElement: () => ({ getContext: () => gl }) },
  WebSocket: class {},
  requestAnimationFrame() {},
  setTimeout() {},
  navigator: { xr: { requestSession: async () => fake } },
  XRWebGLLayer: class {},
  encodeURIComponent,
});
vm.runInContext(fs.readFileSync(process.argv[2], "utf8"), context);
// a frame as the headset would run it: the pose is missing so nothing is drawn
const runFrame = () => {
  const next = frames;
  frames = [];
  next.forEach((f) => f(0, { getViewerPose: () => ({ transform: { matrix: head }, views: [] }) }));
};

(async () => {
  // the trigger (0) alone does nothing, and a button down on the way in must be let go first
  fake.inputSources = [pad(5)];
  await enter.click();
  await new Promise((resolve) => setImmediate(resolve));
  runFrame();
  assert.strictEqual(ends, 0, "held on the way in");
  fake.inputSources = [pad(0)];
  runFrame();
  assert.strictEqual(ends, 0, "the trigger alone");
  const model = () => Array.from(vm.runInContext("model", context));
  const centre = () => model().slice(12, 15).map((v) => Math.round(v * 100) / 100);
  assert.deepStrictEqual(centre(), [-1, 1.6, 2], "in front of the head on the first frame");
  // the head turns to face the other way: nothing moves until A or X
  head = [-1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -1, 0, 1, 1.6, 2, 1];
  runFrame();
  assert.deepStrictEqual(centre(), [-1, 1.6, 2], "it stays put");
  fake.inputSources = [pad(4)];
  runFrame();
  assert.strictEqual(ends, 0, "A and X");
  assert.deepStrictEqual(centre(), [1, 1.6, 4], "A or X brings it in front again");
  head = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 1, 0, 1];
  spaceListeners.reset();
  runFrame();
  assert.deepStrictEqual(centre(), [0, 1, -2], "a recentre of the headset does too");
  fake.inputSources = [pad(), pad(5)];
  runFrame();
  assert.strictEqual(ends, 1, "B or Y");
  assert.strictEqual(enter.disabled, false, "the button is back");
  assert.strictEqual(vm.runInContext("session", context), null);

  // the grip's own event, and the grip button, on a second visit
  await enter.click();
  await new Promise((resolve) => setImmediate(resolve));
  listeners.squeeze();
  assert.strictEqual(ends, 2, "a squeeze");
  await enter.click();
  await new Promise((resolve) => setImmediate(resolve));
  fake.inputSources = [pad(1)];
  runFrame();
  fake.inputSources = [pad()];
  runFrame();
  fake.inputSources = [pad(1)];
  runFrame();
  assert.strictEqual(ends, 3, "a grip button");
})().then(() => process.exit(0), (e) => {
  console.error(e);
  process.exit(1);
});
