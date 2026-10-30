// Opt-in real-game acceptance through the actual C/C++ DevTools extension.
// Config names local-only Wasm/discs/source roots and independently expected C
// expressions. No game-derived bytes are checked into this repository.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {createReadStream} from 'node:fs';
import {mkdtemp, readFile, rm, stat} from 'node:fs/promises';
import path from 'node:path';
import {pathToFileURL} from 'node:url';

const config = JSON.parse(await readFile(process.argv[2], 'utf8'));
// Keep private disc paths and optional renderer locations out of case files.
if (process.env.PSXW_DWARF_DISC_DIR) config.roots['/disc'] = process.env.PSXW_DWARF_DISC_DIR;
if (process.env.PSXW_DWARF_RENDERER_DIR) config.roots['/pkg'] = process.env.PSXW_DWARF_RENDERER_DIR;
const extension = process.env.PSXW_DWARF_EXTENSION;
assert.ok(extension && process.env.PSXW_CHROMIUM, 'set PSXW_DWARF_EXTENSION and PSXW_CHROMIUM');
const moduleName = process.env.PSXW_PUPPETEER_MODULE;
const {default: puppeteer} = await import(moduleName ? pathToFileURL(path.resolve(moduleName)).href : 'puppeteer');
const profile = await mkdtemp(path.join(path.dirname(path.resolve(config.wasm)), '.game-browser-'));
const mappings = Object.entries(config.roots).map(([url, file]) => [url, path.resolve(file)]);
const html = `<script type="module">
import {Disc, userData} from '/web/disc.js';
import init, {WebRenderer} from '/pkg/psxw_host_web.js';
import {ModLoader} from '/web/mods.js';
import {Calls, ScriptHost} from '/web/script.js';
window.load = async config => {
  let e, loader, scripts, frame = 0;
  const events = [], logs = [];
  const files = new Map(await Promise.all(config.files.map(async f => [f, new Uint8Array(await (await fetch('/disc/' + encodeURIComponent(f))).arrayBuffer())])));
  const directory = {getFileHandle: async name => ({
    getFile: async () => ({text: async () => await (await fetch('/disc/' + encodeURIComponent(name))).text()}),
    createSyncAccessHandle: async () => ({getSize: () => files.get(name).length,
      read: (out, {at}) => { const bytes = files.get(name).subarray(at, at + out.length); out.set(bytes); return bytes.length; }})
  })};
  const disc = await Disc.open(directory, config.cue);
  const replay = config.replay ? new DataView(await (await fetch('/replay.rpl')).arrayBuffer()) : null;
  await init({module_or_path: '/pkg/psxw_host_web_bg.wasm'});
  const renderer = new WebRenderer(new OffscreenCanvas(640, 480));
  const mem = () => e.memory.buffer;
  const bytes = (p, n) => new Uint8Array(mem(), p >>> 0, n);
  const text = (p, n) => new TextDecoder().decode(bytes(p, n));
  const raw = new Uint8Array(2352);
  const host = {
    vblank: f => {
      frame = Number(f);
      const work = () => { scripts?.vblank(); if (frame >= config.frames) throw Error('frame limit'); };
      if (scripts) scripts.asHost(work); else work();
    },
    wait_vblank() {},
    log: (level, p, n) => logs.push({frame, level, text: text(p, n)}),
    trap: (code, p, n) => { throw Error('runtime trap ' + code + ': ' + text(p, n)); },
    disc_info: p => { disc.info(new DataView(mem()), p >>> 0); return 1; },
    disc_read: (lba, count, mode, dst) => {
      for (let i = 0, at = dst >>> 0; i < count; i++) {
        if (!disc.readRaw(lba + i, raw)) return -1;
        const data = mode === 0 ? raw : userData(raw); bytes(at, data.length).set(data); at += data.length;
      }
      return 0;
    },
    pad_state: (port, out) => {
      let buttons = 65535;
      if (replay) {
        if (replay.getUint32(8, true) !== 2) throw Error('expected version 2 replay');
        if (replay.getUint32(60, true) & (1 << (port & 1))) return 0;
        const clock = replay.getUint32(56, true);
        const at = clock ? new DataView(mem()).getUint32((e.guest_window() >>> 0) + (clock & 0x1ffffc), true) : frame;
        if (at < replay.getUint32(16, true)) buttons = replay.getUint16(64 + 4 * at + 2 * (port & 1), true);
      }
      new DataView(mem()).setUint16(out >>> 0, buttons, true); return 2;
    },
    storage_read: () => -1, storage_write: () => 0,
    report: (p, n) => {
      const event = JSON.parse(text(p, n)); events.push(event);
      if (loader && event.event === 'boot') loader.booted = true;
    },
    render_submit: (p, n) => renderer.submit(new Uint32Array(mem().slice(p >>> 0, (p >>> 0) + n))),
    render_readback: (x, y, w, h, dst) => { const data = renderer.readback(x, y, w, h); bytes(dst, data.byteLength).set(new Uint8Array(data.buffer, data.byteOffset, data.byteLength)); },
    present: p => renderer.present(new Uint32Array(mem().slice(p >>> 0, (p >>> 0) + 20))),
    gpu_record_begin: () => 0, gpu_record() {}, spu_record_begin: () => 0, spu_record() {}, audio() {},
  };
  const base = await WebAssembly.instantiateStreaming(fetch('/game.wasm'), {host});
  e = base.instance.exports;
  if (config.mods?.length) {
    loader = ModLoader.forBase(base.module, e);
    if (!loader) throw Error('script acceptance requires a moddable debug base');
    scripts = new ScriptHost({exports: e, loader, snapshot: await (await fetch('/snapshot.json')).json(),
      windowBase: e.guest_window() >>> 0, calls: new Calls(e),
      log: text => logs.push({frame, text}), status() {}});
    for (const url of config.mods) {
      scripts.queue({...await scripts.importMod(new URL(url, location.href).href), fatal: true}).catch(() => {});
      scripts.vblank();
    }
  }
  window.dwarfState = () => {
    const view = new DataView(mem()), base = e.guest_window() >>> 0;
    return {frame, thread: view.getUint32(base + 0xe000, true), sp: e.__guest_sp?.value >>> 0,
      words: Object.fromEntries(config.memory.map(a => [a, view.getUint32(base + (a & 0x1ffffc), true)]).concat(
        (config.indirectMemory ?? []).map(({pointer, offset}) => [pointer + '+' + offset,
          view.getUint32(base + ((view.getUint32(base + (pointer & 0x1ffffc), true) + offset) & 0x1ffffc), true)])))};
  };
  window.run = () => {
    const opts = e.psxw_alloc(24) >>> 0;
    new Uint8Array(mem(), opts, 24).fill(0);
    new DataView(mem()).setUint32(opts, config.instantCd ? 1 : 0, true);
    let reason = 'returned';
    try { e.psxw_boot(opts); } catch (error) { reason = error.message; }
    e.psxw_report();
    return {reason, frame, events, logs, ram: String(e.psxw_ram_hash())};
  };
};
</script>`;
const server = createServer(async (req, res) => {
  try {
    res.setHeader('Access-Control-Allow-Origin', '*');
    res.setHeader('Cross-Origin-Resource-Policy', 'cross-origin');
    const url = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
    if (url === '/') { res.setHeader('Content-Type', 'text/html'); res.end(html); return; }
    const mapping = mappings.find(([prefix]) => url === prefix || url.startsWith(prefix + '/'));
    assert.ok(mapping, `unmapped ${url}`);
    const file = path.resolve(mapping[1], '.' + url.slice(mapping[0].length));
    assert.ok(file === mapping[1] || file.startsWith(mapping[1] + path.sep));
    const size = (await stat(file)).size;
    res.setHeader('Content-Length', size);
    res.setHeader('Content-Type', file.endsWith('.wasm') ? 'application/wasm' : file.endsWith('.js') ? 'text/javascript' : 'text/plain');
    createReadStream(file).pipe(res);
  } catch { res.writeHead(404).end(); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await puppeteer.launch({executablePath: process.env.PSXW_CHROMIUM, userDataDir: profile, headless: true,
    args: ['--no-sandbox', '--remote-allow-origins=*', '--enable-unsafe-swiftshader', '--js-flags=--max-old-space-size=8192', `--disable-extensions-except=${extension}`, `--load-extension=${extension}`],
    ignoreDefaultArgs: ['--disable-extensions']});
  const page = await browser.newPage();
  page.setDefaultTimeout(180000);
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.waitForFunction(() => typeof load === 'function');
  const cdp = await page.createCDPSession();
  await cdp.send('Debugger.enable');
  const {targetInfo} = await cdp.send('Target.getTargetInfo');
  const devtools = await browser.newPage();
  devtools.setDefaultTimeout(180000);
  await devtools.goto(`devtools://devtools/bundled/devtools_app.html?ws=127.0.0.1:${new URL(browser.wsEndpoint()).port}/devtools/page/${targetInfo.targetId}`);
  await page.evaluate(config => load(config), config);
  await devtools.waitForFunction(async sources => {
    const W = await import('./models/workspace/workspace.js');
    return sources.every(suffix => W.Workspace.WorkspaceImpl.instance().uiSourceCodes().some(s => s.url().endsWith(suffix)));
  }, {}, config.breakpoints.map(b => b.source));
  await devtools.evaluate(async breakpoints => {
    const W = await import('./models/workspace/workspace.js');
    const B = await import('./models/breakpoints/breakpoints.js');
    window.dwarfBreakpoints = [];
    for (const b of breakpoints) {
      const source = W.Workspace.WorkspaceImpl.instance().uiSourceCodes().find(s => s.url().endsWith(b.source));
      window.dwarfBreakpoints.push(await B.BreakpointManager.BreakpointManager.instance().setBreakpoint(source, b.line - 1, undefined, '', true, false));
    }
  }, config.breakpoints);
  const hits = [], counts = config.breakpoints.map(() => 0);
  let correct = 0, unavailable = 0;
  let failure;
  cdp.on('Debugger.paused', async pause => {
    try {
      await devtools.waitForFunction(async () => {
        const SDK = await import('./core/sdk/sdk.js');
        return SDK.TargetManager.TargetManager.instance().models(SDK.DebuggerModel.DebuggerModel).some(m => m.isPaused());
      });
      const state = (await cdp.send('Runtime.evaluate', {expression: 'dwarfState()', returnByValue: true})).result.value;
      const hit = await devtools.evaluate(async breakpoints => {
        const SDK = await import('./core/sdk/sdk.js');
        const Bindings = await import('./models/bindings/bindings.js');
        const model = SDK.TargetManager.TargetManager.instance().models(SDK.DebuggerModel.DebuggerModel).find(m => m.isPaused());
        const frame = model.debuggerPausedDetails().callFrames[0];
        const location = await Bindings.DebuggerWorkspaceBinding.DebuggerWorkspaceBinding.instance().rawLocationToUILocation(frame.location());
        const index = breakpoints.findIndex(b => location?.uiSourceCode.url().endsWith(b.source) && location.lineNumber + 1 === b.line);
        const values = {};
        for (const {expression} of breakpoints[index]?.checks ?? []) {
          const result = await frame.evaluate({expression, objectGroup: 'dwarf-game', includeCommandLineAPI: false, silent: true, returnByValue: false, generatePreview: true, throwOnSideEffect: false});
          values[expression] = result.error ?? result.object?.description ?? result.exceptionDetails ?? 'unavailable';
        }
        return {index, source: location?.uiSourceCode.url(), line: location?.lineNumber + 1, function: frame.functionName, values};
      }, config.breakpoints);
      hits.push({...hit, state, rawLocation: pause.callFrames[0].location});
      assert.ok(hit.index >= 0, 'unexpected pause: ' + JSON.stringify(pause));
      const caller = config.breakpoints[hit.index].caller;
      if (caller) assert.ok(pause.callFrames.slice(1).some(f => f.functionName.includes(caller)), 'expected C caller ' + caller);
      for (const check of config.breakpoints[hit.index].checks) {
        const expected = check.memory !== undefined ? state.words[check.memory] : check.state ? state[check.state] : check.expected;
        const actual = hit.values[check.expression];
        if (check.optional && /no location|optimized out|not available/.test(actual)) { unavailable++; continue; }
        assert.equal(actual, String(expected), JSON.stringify({check, hit, state}));
        correct++;
      }
      if (++counts[hit.index] >= (config.breakpoints[hit.index].hits ?? 2)) {
        await devtools.evaluate(index => dwarfBreakpoints[index].remove(false), hit.index);
      }
    } catch (error) {
      failure ??= error;
      await devtools.evaluate(() => Promise.all(dwarfBreakpoints.map(b => b.remove(false)))).catch(() => {});
    }
    finally { await cdp.send('Debugger.resume').catch(() => {}); }
  });
  const result = await page.evaluate(() => run());
  console.log(JSON.stringify({browser: await browser.version(), result, hits, counts, availability: {correct, unavailable}}, null, 2));
  if (failure) throw failure;
  assert.equal(result.reason, 'frame limit');
  assert.ok(counts.every((n, i) => n >= (config.breakpoints[i].hits ?? 2)), 'every requested source breakpoint must be reached');
  if (config.minThreads) assert.ok(new Set(hits.map(h => h.state.thread)).size >= config.minThreads);
  if (config.mods?.length) {
    for (const action of ['enter', 'leave']) assert.ok(result.logs.some(l => l.text.includes(action + ' threaded C callee')), 'script hook must execute ' + action);
  }
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
  await rm(profile, {recursive: true, force: true});
}
