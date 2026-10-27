// @ts-check
// conflict-b's script twin: replaces Rand outright, and marks the state
// with 0xB so a trace shows whose Rand ran.

export default mod(({ game, mem, hook }) => {
  const state = game.addr.gRandomState;
  return {
    id: "conflict-b-js",
    version: "0.1.0",
    hooks: [
      hook(game.Rand, {
        replace() {
          mem.setU32(state, 0xb0000000 | ((mem.u32(state) + 1) & 0xffff));
          return 0;
        },
      }),
    ],
  };
});
