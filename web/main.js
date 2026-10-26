// The main thread (M5): disc import into OPFS, memory-card import and
// export (cards.js), input polling into a SharedArrayBuffer, audio setup
// (silence until M6), and the Worker that runs the game. With "Record a
// replay" ticked, the Worker records the pads against the game's frame
// counter (game.json, from the manifest); "Save replay" downloads it as a
// psxw-abi replay, which `psxw run`, the oracle and `web-test` take from
// games/<id>/replays/<name>.rpl. Query parameters for tests: ?cue=NAME&frames=N&verify
// &replay=URL&hash=F,F,...&autostart; ?discurl=URL imports the .cue at URL
// (and the .bin files it names, beside it) into OPFS first; ?post=NAME
// POSTs the hashes to /result?browser=NAME when the run stops.

import { exportCard, importCard } from "./cards.js";

const status = document.getElementById("status");
const say = (t) => (status.textContent = t);
const params = new URLSearchParams(location.search);

if (!crossOriginIsolated) say("This page needs cross-origin isolation (COOP/COEP headers): serve it with `psxw serve`.");

// Pad words are active low (psxw-abi replay): bit order Select, L3, R3,
// Start, Up, Right, Down, Left, L2, R2, L1, R1, Triangle, Circle, Cross,
// Square.
const KEYS = {
  ShiftRight: 0, Enter: 3, ArrowUp: 4, ArrowRight: 5, ArrowDown: 6, ArrowLeft: 7,
  KeyQ: 8, KeyE: 9, KeyA: 10, KeyD: 11, KeyW: 12, KeyX: 13, KeyS: 14, KeyZ: 15,
};
// Standard Gamepad API layout to pad bits.
const BUTTONS = { 8: 0, 10: 1, 11: 2, 9: 3, 12: 4, 15: 5, 13: 6, 14: 7, 6: 8, 7: 9, 4: 10, 5: 11, 3: 12, 1: 13, 0: 14, 2: 15 };

// [seq, port 1 buttons, port 2 buttons or -1, save-replay requests]
const padBuffer = new SharedArrayBuffer(16);
const pad = new Int32Array(padBuffer);
Atomics.store(pad, 1, 0xffff);
Atomics.store(pad, 2, -1);
const held = new Set();
addEventListener("keydown", (e) => { if (e.code in KEYS) { held.add(e.code); e.preventDefault(); } });
addEventListener("keyup", (e) => held.delete(e.code));

function pollInput() {
  let word = 0xffff;
  for (const code of held) word &= ~(1 << KEYS[code]);
  for (const gp of navigator.getGamepads ? navigator.getGamepads() : []) {
    if (!gp) continue;
    gp.buttons.forEach((b, i) => { if (b.pressed && i in BUTTONS) word &= ~(1 << BUTTONS[i]); });
  }
  Atomics.store(pad, 1, word);
  Atomics.add(pad, 0, 1);
  requestAnimationFrame(pollInput);
}

async function importDisc(files) {
  const root = await navigator.storage.getDirectory();
  await root.removeEntry("disc", { recursive: true }).catch(() => {});
  const dir = await root.getDirectoryHandle("disc", { create: true });
  let cue = null;
  for (const f of files) {
    say(`copying ${f.name} into OPFS…`);
    const w = await (await dir.getFileHandle(f.name, { create: true })).createWritable();
    await f.stream().pipeTo(w);
    if (f.name.toLowerCase().endsWith(".cue")) cue = f.name;
  }
  if (!cue) throw new Error("no .cue among the files");
  localStorage.setItem("psxw.cue", cue);
  return cue;
}

/** Fetch a .cue and its .bin files over HTTP into OPFS (test hook). */
async function importDiscUrl(url) {
  const cueText = await (await fetch(url)).text();
  const base = new URL(url, location.href);
  const names = [...cueText.matchAll(/^\s*FILE\s+"?([^"]+?)"?\s+\S+\s*$/gm)].map((m) => m[1]);
  const files = [new File([cueText], base.pathname.split("/").pop())];
  for (const n of names) {
    say(`fetching ${n}…`);
    files.push(new File([await (await fetch(new URL(n, base))).blob()], n));
  }
  return importDisc(files);
}

async function startAudio() {
  // Silence until M6 fills the ring; the worklet's clock drives nothing yet.
  try {
    const ctx = new AudioContext({ sampleRate: 44100 });
    await ctx.audioWorklet.addModule("audio.js");
    const ring = new SharedArrayBuffer(4 * 4096 + 8);
    new AudioWorkletNode(ctx, "psxw-audio", { processorOptions: { ring }, outputChannelCount: [2] }).connect(ctx.destination);
    return ring;
  } catch (e) {
    console.warn("audio unavailable", e);
    return null;
  }
}

async function gameInfo() {
  try {
    return await (await fetch("game.json")).json();
  } catch {
    return {};
  }
}

const saveReplay = document.getElementById("save-replay");
saveReplay.addEventListener("click", () => Atomics.add(pad, 3, 1));

async function start(cue, audioRing) {
  const game = await gameInfo();
  const record = document.getElementById("record").checked || params.has("record");
  if (record && game.frame_counter == null) say("this bundle names no frame counter; recording is off");
  const canvas = document.getElementById("screen").transferControlToOffscreen();
  const worker = new Worker("worker.js", { type: "module" });
  const options = {
    instantCd: document.getElementById("instant").checked || params.has("instant"),
    frames: params.has("frames") ? parseInt(params.get("frames"), 10) : 0,
    verify: params.has("verify"),
    replay: params.get("replay"),
    hashAt: (params.get("hash") || "").split(",").filter(Boolean).map(Number),
    dev: params.has("dev"),
    record: record && game.frame_counter != null,
    clock: game.frame_counter,
    gameId: game.id || "",
  };
  saveReplay.disabled = !options.record;
  worker.onmessage = (e) => {
    const m = e.data;
    if (m.type === "status") say(m.text);
    else if (m.type === "replay" && params.has("post")) {
      fetch(`/result?browser=${params.get("post") || params.get("browser")}-replay`, { method: "POST", body: m.bytes });
    } else if (m.type === "replay") {
      const a = document.createElement("a");
      a.href = URL.createObjectURL(new Blob([m.bytes], { type: "application/octet-stream" }));
      a.download = `${options.gameId || "game"}-${new Date().toISOString().replace(/[:.]/g, "-")}.rpl`;
      a.click();
      URL.revokeObjectURL(a.href);
      say(`replay saved: ${m.frames} frames`);
    }
    else if (m.type === "log") console.log(`[${m.frame}] ${m.text}`);
    else if (m.type === "hashes") {
      window.psxwHashes = m;
      if (params.has("post")) {
        fetch(`/result?browser=${params.get("post") || params.get("browser")}`, { method: "POST", body: JSON.stringify(m) });
      }
    }
    else if (m.type === "stopped") {
      say(`stopped at frame ${m.frame ?? "?"}: ${m.reason}`);
      window.psxwStopped = m;
      if (params.has("post") && !window.psxwHashes) {
        fetch(`/result?browser=${params.get("post") || params.get("browser")}`, { method: "POST", body: JSON.stringify({ error: m.reason, frame: m.frame }) });
      }
    }
  };
  worker.postMessage({ type: "start", canvas, cue, padBuffer, audioRing, options }, [canvas]);
}

const discInput = document.getElementById("disc");
const startButton = document.getElementById("start");
let cue = params.get("cue") || localStorage.getItem("psxw.cue");
if (cue) {
  startButton.disabled = false;
  say(`disc ${cue} is in OPFS; press Start`);
}
discInput.addEventListener("change", async () => {
  try {
    cue = await importDisc([...discInput.files]);
    startButton.disabled = false;
    say(`imported ${cue}; press Start`);
  } catch (e) {
    say(String(e));
  }
});
// Cards are read when the game starts and written as it saves: importing
// one into a running game would be overwritten, so only before Start.
let running = false;
for (const slot of [0, 1]) {
  const input = document.getElementById(`card${slot}`);
  input.addEventListener("change", async () => {
    try {
      if (running) throw new Error("import cards before pressing Start");
      await importCard(slot, new Uint8Array(await input.files[0].arrayBuffer()));
      say(`imported ${input.files[0].name} into slot ${slot + 1}`);
    } catch (e) {
      say(String(e));
    }
    input.value = "";
  });
  document.getElementById(`export${slot}`).addEventListener("click", async () => {
    const bytes = await exportCard(slot);
    if (!bytes) return say(`slot ${slot + 1} has no card yet`);
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([bytes], { type: "application/octet-stream" }));
    a.download = `card${slot + 1}.mcr`;
    a.click();
    URL.revokeObjectURL(a.href);
  });
}

startButton.addEventListener("click", async () => {
  startButton.disabled = true;
  running = true;
  const ring = await startAudio();
  requestAnimationFrame(pollInput);
  await start(cue, ring);
});
if (params.has("discurl")) {
  importDiscUrl(params.get("discurl"))
    .then((c) => {
      cue = c;
      startButton.disabled = false;
      if (params.has("autostart")) startButton.click();
    })
    .catch((e) => {
      say(String(e));
      if (params.has("post")) fetch(`/result?browser=${params.get("post") || params.get("browser")}`, { method: "POST", body: JSON.stringify({ error: String(e) }) });
    });
} else if (params.has("autostart") && cue) startButton.click();
