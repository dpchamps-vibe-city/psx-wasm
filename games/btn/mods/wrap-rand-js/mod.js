// @ts-check
// wrap-rand's script twin: before, around and after on Rand; every result
// is checked against the original's range.

export default mod(({ game, hook, log }) => {
  let calls = 0;
  let results = 0;
  let bad = 0;
  return {
    id: "wrap-rand-js",
    version: "0.1.0",
    hooks: [
      hook(game.Rand, {
        before() {
          calls++;
        },
        around(original, limit) {
          const r = original(limit) >>> 0;
          if ((limit & 0xffff) !== 0 && r >= (limit & 0xffff)) bad++;
          return r;
        },
        after() {
          results++;
          if (results === 1000) log(`rand: ${calls} calls, ${results} results, ${bad} out of range`);
        },
      }),
    ],
  };
});
