// Real script-layer control: the C callee's breakpoints/values must survive
// entry through the normal ScriptHost/ModLoader hook chain on a debug base.
import {mod} from '/web/script.js';
export default mod(({game, hook, log}) => ({
  id: 'dwarf-acceptance',
  hooks: [hook(game.at('exe', 0x8013a4ac), {
    before() { log('enter threaded C callee'); },
    after() { log('leave threaded C callee'); },
  })],
}));
