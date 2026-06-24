// ferrisboy web frontend.
//
// Responsibilities (the Rust side stays thin — see web/src/lib.rs):
//   - load the wasm module and construct an `Emulator` from ROM bytes
//   - run one emulator frame per requestAnimationFrame and blit it to a canvas
//   - feed audio through a ScriptProcessor-backed ring buffer at 44100 Hz
//   - map the keyboard onto the 8 Game Boy buttons
//   - persist battery-backed save RAM to localStorage, keyed by ROM title

import init, { Emulator } from "./pkg/ferrisboy_web.js";

const SCREEN_W = 160;
const SCREEN_H = 144;
const SAMPLE_RATE = 44100;

// --- DOM ------------------------------------------------------------------
const canvas = document.getElementById("screen");
const ctx = canvas.getContext("2d", { alpha: false });
const statusEl = document.getElementById("status");
const hintEl = document.getElementById("hint");
const fileInput = document.getElementById("rom");
const pauseBtn = document.getElementById("pause");
const resetBtn = document.getElementById("reset");
const stage = document.getElementById("stage");

// One reusable ImageData for the native-resolution frame; CSS scales it up.
const imageData = ctx.createImageData(SCREEN_W, SCREEN_H);

function setStatus(msg, isError = false) {
  statusEl.textContent = msg;
  statusEl.classList.toggle("error", isError);
}

// --- Audio: a small ring buffer drained by a ScriptProcessorNode ----------
// The emulator produces interleaved stereo f32 at 44100 Hz. We push each
// frame's samples into a ring buffer and let the audio node pull from it,
// outputting silence on underrun. If the AudioContext can't run at 44100 we
// nearest-neighbour resample on the way out — good enough for a toy.
const audio = {
  ctx: null,
  node: null,
  ringL: new Float32Array(SAMPLE_RATE), // ~1s capacity per channel
  ringR: new Float32Array(SAMPLE_RATE),
  size: 0,
  read: 0,
  write: 0,
  ratio: 1, // output sampleRate / 44100
};

function ensureAudio() {
  if (audio.ctx) {
    if (audio.ctx.state === "suspended") audio.ctx.resume();
    return;
  }
  const AC = window.AudioContext || window.webkitAudioContext;
  if (!AC) return;
  // Ask for 44100 explicitly; browsers may ignore it, hence the resample ratio.
  let context;
  try {
    context = new AC({ sampleRate: SAMPLE_RATE });
  } catch (_) {
    context = new AC();
  }
  audio.ctx = context;
  audio.ratio = context.sampleRate / SAMPLE_RATE;

  const node = context.createScriptProcessor(2048, 0, 2);
  node.onaudioprocess = (e) => {
    const outL = e.outputBuffer.getChannelData(0);
    const outR = e.outputBuffer.getChannelData(1);
    const n = outL.length;
    for (let i = 0; i < n; i++) {
      if (audio.size > 0) {
        outL[i] = audio.ringL[audio.read];
        outR[i] = audio.ringR[audio.read];
        // Advance the read head by `ratio` source samples per output sample
        // (nearest-neighbour resample when the context isn't at 44100 Hz).
        audio._frac = (audio._frac || 0) + audio.ratio;
        while (audio._frac >= 1 && audio.size > 0) {
          audio.read = (audio.read + 1) % audio.ringL.length;
          audio.size--;
          audio._frac -= 1;
        }
      } else {
        // Underrun: output silence and reset the resample phase.
        outL[i] = 0;
        outR[i] = 0;
        audio._frac = 0;
      }
    }
  };
  node.connect(context.destination);
  audio.node = node;
}

function pushAudio(samples) {
  if (!audio.ctx) return;
  const cap = audio.ringL.length;
  for (let i = 0; i + 1 < samples.length; i += 2) {
    if (audio.size >= cap) {
      // Buffer full (tab was throttled). Drop oldest to stay near-realtime.
      audio.read = (audio.read + 1) % cap;
      audio.size--;
    }
    audio.ringL[audio.write] = samples[i];
    audio.ringR[audio.write] = samples[i + 1];
    audio.write = (audio.write + 1) % cap;
    audio.size++;
  }
}

// --- Emulator loop --------------------------------------------------------
let emu = null;
let romBytes = null; // keep for Reset
let saveKey = null;
let running = false;
let rafId = 0;
let saveTimer = 0;

const SAVE_PREFIX = "ferrisboy.save.";

function loadSavedRam(title) {
  try {
    const b64 = localStorage.getItem(SAVE_PREFIX + title);
    if (!b64) return null;
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch (_) {
    return null;
  }
}

function persistSaveIfDirty() {
  if (!emu || !saveKey) return;
  if (!emu.isSaveDirty()) return;
  const data = emu.save_data();
  if (!data) {
    emu.markSaved();
    return;
  }
  try {
    let bin = "";
    for (let i = 0; i < data.length; i++) bin += String.fromCharCode(data[i]);
    localStorage.setItem(saveKey, btoa(bin));
    emu.markSaved();
  } catch (_) {
    // localStorage may be full or disabled; skip silently.
  }
}

function stopLoop() {
  running = false;
  if (rafId) cancelAnimationFrame(rafId);
  rafId = 0;
}

function startLoop() {
  if (running) return;
  running = true;
  pauseBtn.textContent = "Pause";
  const tick = () => {
    if (!running || !emu) return;
    emu.run_frame();

    const rgba = emu.frame_rgba();
    imageData.data.set(rgba);
    ctx.putImageData(imageData, 0, 0);

    pushAudio(emu.take_audio());

    rafId = requestAnimationFrame(tick);
  };
  rafId = requestAnimationFrame(tick);
}

function startEmulator(bytes) {
  stopLoop();

  // Probe the title (needs a temporary instance) to find any saved RAM.
  let savedRam = null;
  let title = "";
  try {
    const probe = new Emulator(bytes);
    title = probe.title();
    probe.free();
    savedRam = loadSavedRam(title);
  } catch (e) {
    setStatus("Unsupported or invalid ROM: " + e, true);
    return;
  }

  try {
    emu = savedRam
      ? Emulator.newWithSave(bytes, savedRam)
      : new Emulator(bytes);
  } catch (e) {
    setStatus("Unsupported or invalid ROM: " + e, true);
    return;
  }

  romBytes = bytes;
  saveKey = SAVE_PREFIX + title;

  hintEl.classList.add("hidden");
  pauseBtn.disabled = false;
  resetBtn.disabled = false;
  setStatus(
    `Playing: ${title || "(untitled ROM)"}${savedRam ? " — save loaded" : ""}`
  );

  ensureAudio();
  startLoop();

  if (saveTimer) clearInterval(saveTimer);
  saveTimer = setInterval(persistSaveIfDirty, 1000);
}

async function loadRomFile(file) {
  if (!file) return;
  setStatus(`Loading ${file.name}…`);
  const buf = await file.arrayBuffer();
  startEmulator(new Uint8Array(buf));
}

// --- Input ----------------------------------------------------------------
// Map keyboard keys to button indices (see button_from_index in lib.rs):
// 0 Right, 1 Left, 2 Up, 3 Down, 4 A, 5 B, 6 Select, 7 Start.
const KEY_MAP = {
  ArrowRight: 0,
  ArrowLeft: 1,
  ArrowUp: 2,
  ArrowDown: 3,
  z: 4,
  Z: 4,
  x: 5,
  X: 5,
  Enter: 7,
  Shift: 6,
};

function handleKey(e, pressed) {
  const idx = KEY_MAP[e.key];
  if (idx === undefined) return;
  if (e.repeat) {
    e.preventDefault();
    return;
  }
  // Sound needs a user gesture to start.
  ensureAudio();
  if (e.key.startsWith("Arrow")) e.preventDefault();
  if (emu) emu.set_button(idx, pressed);
}

window.addEventListener("keydown", (e) => handleKey(e, true));
window.addEventListener("keyup", (e) => handleKey(e, false));

// --- UI wiring ------------------------------------------------------------
fileInput.addEventListener("change", (e) => {
  const file = e.target.files && e.target.files[0];
  if (file) loadRomFile(file);
});

pauseBtn.addEventListener("click", () => {
  if (!emu) return;
  if (running) {
    stopLoop();
    pauseBtn.textContent = "Resume";
    setStatus("Paused.");
  } else {
    ensureAudio();
    startLoop();
    setStatus("Playing.");
  }
});

resetBtn.addEventListener("click", () => {
  if (romBytes) {
    persistSaveIfDirty();
    startEmulator(romBytes);
  }
});

// Drag & drop anywhere on the stage (and the whole window).
function preventDefaults(e) {
  e.preventDefault();
  e.stopPropagation();
}
["dragenter", "dragover", "dragleave", "drop"].forEach((ev) => {
  window.addEventListener(ev, preventDefaults);
});
["dragenter", "dragover"].forEach((ev) =>
  stage.addEventListener(ev, () => stage.classList.add("dragover"))
);
["dragleave", "drop"].forEach((ev) =>
  stage.addEventListener(ev, () => stage.classList.remove("dragover"))
);
window.addEventListener("drop", (e) => {
  const file = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
  if (file) loadRomFile(file);
});

// Persist on tab close so the last second of play isn't lost.
window.addEventListener("beforeunload", persistSaveIfDirty);

// Resume audio on any click (autoplay policy fallback).
window.addEventListener("click", ensureAudio);

// --- Boot -----------------------------------------------------------------
init()
  .then(() => {
    setStatus("Ready — drop in a .gb ROM to start.");
  })
  .catch((e) => {
    setStatus("Failed to load the emulator module: " + e, true);
  });
