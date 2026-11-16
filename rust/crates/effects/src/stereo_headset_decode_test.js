"use strict";
// Runs the headset page's script against stand-ins for the browser and checks that a picture still being decoded never comes back after black.
const assert = require("assert");
const fs = require("fs");
const vm = require("vm");

const pending = [];
const sockets = [];
let closed = 0;
const element = () => ({ getContext: () => ({ drawImage() {}, fillRect() {} }), addEventListener() {}, textContent: "", width: 0, height: 0, disabled: false });
class FakeSocket {
  constructor() {
    sockets.push(this);
  }
}
const context = vm.createContext({
  location: { hash: "#token", host: "host" },
  document: { getElementById: element, createElement: element },
  WebSocket: FakeSocket,
  createImageBitmap: () => new Promise((resolve) => pending.push(resolve)),
  requestAnimationFrame() {},
  setTimeout() {},
  navigator: {},
  encodeURIComponent,
});
vm.runInContext(fs.readFileSync(process.argv[2], "utf8"), context);
const bitmap = () => vm.runInContext("bitmap", context);
const picture = () => ({ close() { closed++; }, width: 1, height: 1 });
const tick = () => new Promise((resolve) => setImmediate(resolve));

(async () => {
  const socket = sockets[0];
  // a picture decoded without anything in between is shown
  const first = picture();
  socket.onmessage({ data: {} });
  pending.shift()(first);
  await tick();
  assert.strictEqual(bitmap(), first);

  // black arrives while a picture is decoding: the picture is dropped and closed, not shown
  socket.onmessage({ data: {} });
  socket.onmessage({ data: "blank" });
  assert.strictEqual(bitmap(), null);
  const late = picture();
  pending.shift()(late);
  await tick();
  assert.strictEqual(bitmap(), null, "black stays");
  assert.strictEqual(closed, 2, "the shown picture and the late one were closed");

  // the same when the socket closes, and a picture waiting behind the one decoding is dropped too
  socket.onmessage({ data: {} });
  socket.onmessage({ data: {} });
  socket.onclose();
  const lateAgain = picture();
  pending.shift()(lateAgain);
  await tick();
  assert.strictEqual(bitmap(), null);
  assert.strictEqual(pending.length, 0, "nothing decodes after the close");

  // and the next picture after black is shown as usual
  const next = picture();
  socket.onmessage({ data: {} });
  pending.shift()(next);
  await tick();
  assert.strictEqual(bitmap(), next);
})().then(() => process.exit(0), (e) => {
  console.error(e);
  process.exit(1);
});
