// Raw V8 state control, not a substitute for the C/C++ DevTools extension.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const inspector = require('node:inspector');
const [file, pointJSON] = process.argv.slice(2);
const point = JSON.parse(pointJSON);
const session = new inspector.Session();
session.connect();
function post(method, params = {}) {
  let result, error;
  session.post(method, params, (e, r) => { error = e; result = r; });
  if (error) throw error;
  assert.notEqual(result, undefined, `${method} must complete while paused`);
  return result;
}
post('Debugger.enable');
let script;
session.on('Debugger.scriptParsed', ({params}) => {
  if (params.scriptLanguage === 'WebAssembly') script = params;
});
const values = [];
let pauseError;
session.on('Debugger.paused', ({params}) => {
  try {
    const frame = params.callFrames[0];
    const scope = frame.scopeChain.find(s => s.type === (point.kind === 'stack' ? 'wasm-expression-stack' : 'local'));
    assert.ok(scope, 'expected wasm scope');
    let properties = post('Runtime.getProperties', {objectId: scope.object.objectId, ownProperties: true}).result;
    if (point.kind === 'stack') {
      const stack = properties.find(p => p.name === 'stack');
      assert.ok(stack);
      properties = post('Runtime.getProperties', {objectId: stack.value.objectId, ownProperties: true}).result;
    }
    const item = properties.find(p => p.name === (point.kind === 'stack' ? `${point.index}` : `$var${point.index}`));
    if (!item) {
      values.push(null);
    } else {
      const raw = post('Runtime.getProperties', {objectId: item.value.objectId, ownProperties: true}).result;
      values.push(raw.find(p => p.name === 'value').value.value);
    }
  } catch (error) {
    pauseError = error;
  } finally {
    post('Debugger.resume');
  }
});
let instance, phase = 'normal';
const bios = value => {
  const e = instance.exports;
  if (!e.asyncify_start_unwind) return value + 100;
  if (phase === 'rewind') {
    e.asyncify_stop_rewind();
    phase = 'normal';
    return value + 100;
  }
  assert.equal(phase, 'normal');
  phase = 'unwind';
  e.asyncify_start_unwind(4096);
  return -999;
};
const module_ = new WebAssembly.Module(fs.readFileSync(file));
instance = new WebAssembly.Instance(module_, {psx: {bios}, host: {bios}});
assert.ok(script);
const breakpoint = post('Debugger.setBreakpoint', {
  location: {scriptId: script.scriptId, lineNumber: 0, columnNumber: script.codeOffset + point.pc},
});
assert.equal(breakpoint.actualLocation.columnNumber, script.codeOffset + point.pc);
const memory = new DataView(instance.exports.memory.buffer);
memory.setUint32(4096, 4104, true);
memory.setUint32(4100, 30000, true);
const entry = point.entry ?? 'root';
let result = instance.exports[entry](5), cycles = 0;
while (phase === 'unwind') {
  assert.ok(cycles < 10, 'unwind must terminate');
  instance.exports.asyncify_stop_unwind();
  phase = 'rewind';
  instance.exports.asyncify_start_rewind(4096);
  cycles++;
  result = instance.exports[entry](5);
}
if (pauseError) throw pauseError;
assert.equal(result, entry === 'root' ? 508 : 794);
assert.equal(memory.getUint32(4096, true), 4104);
post('Debugger.disable');
session.disconnect();
const cases = [];
for (const entry of ['root', 'branchy']) {
  for (const input of [0, 1, 5, 100]) {
    let e, phase = 'normal';
    const bios = x => {
      if (!e.asyncify_start_unwind) return x + 100;
      if (phase === 'rewind') { e.asyncify_stop_rewind(); phase = 'normal'; return x + 100; }
      assert.equal(phase, 'normal');
      phase = 'unwind'; e.asyncify_start_unwind(4096); return -999;
    };
    e = new WebAssembly.Instance(module_, {psx: {bios}, host: {bios}}).exports;
    const memory = new DataView(e.memory.buffer);
    memory.setUint32(4096, 4104, true); memory.setUint32(4100, 30000, true);
    let result = e[entry](input), cycles = 0;
    while (phase === 'unwind') {
      assert.ok(++cycles < 10);
      e.asyncify_stop_unwind(); phase = 'rewind'; e.asyncify_start_rewind(4096);
      result = e[entry](input);
    }
    const worker = x => { const before = x + 11; return before * 3 + ((before * 3 + 7) ^ 0x55) + 100; };
    let expected;
    if (entry === 'root') expected = worker(input) + worker(input + 1);
    else {
      let value = (input & 1 ? input + 2 : input - 2) + 100;
      for (let i = 0; i < 2; i++) value += value + i + 100;
      expected = value + input * 13;
    }
    assert.equal(result, expected, `${entry}(${input})`);
    assert.equal(cycles, e.asyncify_start_unwind ? (entry === 'root' ? 2 : 3) : 0);
    assert.equal(memory.getUint32(4096, true), 4104);
    cases.push({entry, input, result, cycles});
  }
}
console.log(JSON.stringify({result, cycles, values, cases}));
