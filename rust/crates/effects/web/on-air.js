"use strict";
// The On Air sign: asks Telescope for the state and waits for the next change, then asks again.

const token = location.hash.slice(1);
const sign = document.getElementById("sign");
const note = document.getElementById("note");
let seq = -1;
let lostTimer = 0;

function show(kind, text, detail) {
  document.body.className = kind;
  sign.textContent = text;
  note.textContent = detail || "";
}

function lost(text) {
  // Brief hiccups keep the last state up; only a lasting loss blanks the sign
  if (!lostTimer) {
    lostTimer = setTimeout(() => show("lost", sign.textContent, text), 5000);
  }
}

async function poll() {
  for (;;) {
    try {
      const response = await fetch("/state?after=" + seq, {
        headers: { Authorization: "Bearer " + token }, cache: "no-store",
      });
      if (response.status === 403) {
        clearTimeout(lostTimer);
        show("lost", "", "This link doesn't work any more. Open the new one from Telescope's On Air settings.");
        return;
      }
      if (!response.ok) throw new Error(String(response.status));
      const state = await response.json();
      clearTimeout(lostTimer);
      lostTimer = 0;
      seq = state.seq;
      show(state.on ? "on" : "off", state.text, state.note);
      if (state.title) document.title = state.title;
    } catch (e) {
      lost("Can't reach Telescope. Trying again…");
      await new Promise((resolve) => setTimeout(resolve, 2000));
    }
  }
}

// Keep the screen on while the sign is showing (browsers drop the lock whenever the page is hidden)
let wakeLock = null;
async function keepAwake() {
  if (!("wakeLock" in navigator) || document.visibilityState !== "visible" || wakeLock) return;
  try {
    wakeLock = await navigator.wakeLock.request("screen");
    wakeLock.addEventListener("release", () => { wakeLock = null; });
  } catch (e) {
    wakeLock = null;
  }
}
document.addEventListener("visibilitychange", keepAwake);

// A tap goes full screen, the way a sign should be
document.addEventListener("click", () => {
  keepAwake();
  if (!document.fullscreenElement && document.documentElement.requestFullscreen) {
    document.documentElement.requestFullscreen().catch(() => {});
  }
});

if (!token) {
  show("lost", "", "Open the full link from Telescope's On Air settings.");
} else {
  keepAwake();
  poll();
}
