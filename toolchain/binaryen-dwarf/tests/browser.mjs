// Opt-in acceptance of named C expressions via the actual DevTools extension.
// Raw CDP wasm scopes are NOT used to evaluate these expressions.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {mkdtemp, readFile, rm} from 'node:fs/promises';
import path from 'node:path';
import {pathToFileURL} from 'node:url';

const wasm = path.resolve(process.argv[2] ?? 'out/threaded-dwarf-tests/ordinary/merged.wasm');
const dir = path.dirname(wasm);
const typed = process.argv[3] === 'typed';
const source = await readFile(path.join(dir, 'fixture.c'), 'utf8');
const line = typed ? source.split('\n').findIndex(s => s.includes('/* typed-stop */')) : 16;
const expressions = typed ? ['sample.count', 'sample.weight', 'sample.scale', 'sample.bits',
  'parts.count', 'parts.weight', 'parts.scale', 'parts.bits', 'ratio', 'big', 'resumed', '*(&sample.count)']
  : ['before', 'resumed', 'stack_value', '*(&stack_value)', 'input'];
const extension = process.env.PSXW_DWARF_EXTENSION;
const executablePath = process.env.PSXW_CHROMIUM;
assert.ok(extension && executablePath, 'set PSXW_DWARF_EXTENSION and PSXW_CHROMIUM');
const moduleName = process.env.PSXW_PUPPETEER_MODULE;
const {default: puppeteer} = await import(moduleName ? pathToFileURL(path.resolve(moduleName)).href : 'puppeteer');
const profile = await mkdtemp(path.join(dir, '.browser-profile-'));
const html = `<script>
window.load = async file => {
  let e, phase = 'normal';
  const bios = x => {
    if (phase === 'rewind') { e.asyncify_stop_rewind(); phase = 'normal'; return x + 100; }
    if (!e.asyncify_start_unwind) return x + 100;
    if (phase !== 'normal') throw Error('invalid unwind phase');
    phase = 'unwind'; e.asyncify_start_unwind(4096); return -999;
  };
  const {instance} = await WebAssembly.instantiateStreaming(fetch(file), {psx: {bios}, host: {bios}});
  e = instance.exports;
  window.run = entry => {
    const memory = new DataView(e.memory.buffer);
    memory.setUint32(4096, 4104, true); memory.setUint32(4100, 30000, true);
    let result = e[entry](5), cycles = 0;
    while (phase === 'unwind') {
      if (++cycles > 10) throw Error('unwind did not terminate');
      e.asyncify_stop_unwind(); phase = 'rewind'; e.asyncify_start_rewind(4096);
      result = e[entry](5);
    }
    return {result, cycles, pointer: memory.getUint32(4096, true)};
  };
};
</script>`;
const server = createServer(async (req, res) => {
  try {
    const name = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
    if (name === '/') { res.setHeader('Content-Type', 'text/html'); res.end(html); return; }
    const file = path.resolve(dir, `.${name}`);
    if (!file.startsWith(`${dir}${path.sep}`)) { res.writeHead(403).end(); return; }
    res.setHeader('Access-Control-Allow-Origin', '*');
    res.setHeader('Cross-Origin-Resource-Policy', 'cross-origin');
    res.setHeader('Content-Type', file.endsWith('.wasm') ? 'application/wasm' : 'text/plain');
    res.end(await readFile(file));
  } catch { res.writeHead(404).end(); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await puppeteer.launch({
    executablePath, userDataDir: profile, headless: true,
    args: ['--no-sandbox', '--remote-allow-origins=*', `--disable-extensions-except=${extension}`, `--load-extension=${extension}`],
    ignoreDefaultArgs: ['--disable-extensions'],
  });
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  const cdp = await page.createCDPSession();
  await cdp.send('Debugger.enable');
  const {targetInfo} = await cdp.send('Target.getTargetInfo');
  const devtools = await browser.newPage();
  await devtools.goto(`devtools://devtools/bundled/devtools_app.html?ws=127.0.0.1:${new URL(browser.wsEndpoint()).port}/devtools/page/${targetInfo.targetId}`);
  await page.evaluate(file => load(file), path.basename(wasm));
  await devtools.waitForFunction(async () => {
    const W = await import('./models/workspace/workspace.js');
    return W.Workspace.WorkspaceImpl.instance().uiSourceCodes().some(s => s.url().endsWith('/fixture.c'));
  }, {timeout: 30000});
  await devtools.evaluate(async line => {
    const W = await import('./models/workspace/workspace.js');
    const B = await import('./models/breakpoints/breakpoints.js');
    const source = W.Workspace.WorkspaceImpl.instance().uiSourceCodes().find(s => s.url().endsWith('/fixture.c'));
    await B.BreakpointManager.BreakpointManager.instance().setBreakpoint(source, line, undefined, '', true, false);
  }, line);
  const hits = [];
  let failure;
  cdp.on('Debugger.paused', async () => {
    try {
      await devtools.waitForFunction(async () => {
        const SDK = await import('./core/sdk/sdk.js');
        return SDK.TargetManager.TargetManager.instance().models(SDK.DebuggerModel.DebuggerModel).some(m => m.isPaused());
      });
      hits.push(await devtools.evaluate(async expressions => {
        const SDK = await import('./core/sdk/sdk.js');
        const model = SDK.TargetManager.TargetManager.instance().models(SDK.DebuggerModel.DebuggerModel).find(m => m.isPaused());
        const frame = model.debuggerPausedDetails().callFrames[0];
        const values = {};
        for (const expression of expressions) {
          const result = await frame.evaluate({expression, objectGroup: 'dwarf-test', includeCommandLineAPI: false, silent: true, returnByValue: false, generatePreview: true, throwOnSideEffect: false});
          values[expression] = result.error ?? result.object?.description ?? result.exceptionDetails ?? 'unavailable';
        }
        return values;
      }, expressions));
    } catch (error) { failure = error; }
    finally { await cdp.send('Debugger.resume'); }
  });
  const result = await page.evaluate(entry => run(entry), typed ? 'typed' : 'root');
  if (failure) throw failure;
  assert.deepEqual(result, {result: typed ? 160 : 508, cycles: typed ? 1 : 2, pointer: 4104});
  if (typed) {
    assert.ok(hits.length > 0, 'typed source breakpoint must be reached');
    const expected = [8, 1.5, 2.25, 4294967303, 10, 3.5, 4.25, 8589934600, 5.5, 42949672967, 105, 8];
    for (const hit of hits) for (const [i, expression] of expressions.entries()) {
      if (expression === 'resumed' && /no location|not available/.test(hit[expression])) continue;
      assert.equal(hit[expression], String(expected[i]), expression);
    }
  } else {
  // Explicit synthetic gaps may give a C line several disjoint executable
  // intervals. Check EVERY physical stop, not just one selected good value.
  assert.deepEqual([...new Set(hits.map(h => h.before))], ['16', '17'], 'both worker calls must hit the C source breakpoint');
  for (const hit of hits) {
    const i = Number(hit.before) - 16;
    for (const [expression, expected] of Object.entries({before: [16, 17], resumed: [198, 211], stack_value: [32, 34], '*(&stack_value)': [32, 34]})) {
      assert.equal(hit[expression], String(expected[i]), `${expression} at worker ${i}`);
    }
  }
  // Correct unavailability is distinct from clang's false 16/17 description.
  // This narrow input check is not game-wide variable-coverage acceptance.
  const inputStatus = hits.map(h => h.input === String(Number(h.before) - 11) ? 'correct'
    : h.input.includes('no location, value may have been optimized out') ? 'unavailable' : 'incorrect');
  assert.ok(inputStatus.every(status => status !== 'incorrect'), 'false input description survived validation');
  }
  console.log(JSON.stringify({browser: await browser.version(), result, hits}, null, 2));
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
  await rm(profile, {recursive: true, force: true});
}
