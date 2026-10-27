// @ts-check
// conflict-a's script twin: replaces Rand outright, and marks the state
// with 0xA so a trace shows whose Rand ran.

export default mod(({ game, mem, hook }) => {
  const state = game.addr.gRandomState;
  return {
    id: "conflict-a-js",
    version: "0.1.0",
    hooks: [
      hook(game.Rand, {
        replace() {
          mem.setU32(state, 0xa0000000 | ((mem.u32(state) + 1) & 0xffff));
          return 0;
        },
      }),
    ],
  };
});
