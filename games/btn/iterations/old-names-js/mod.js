// @ts-check
// A script mod written against btn's decomp at its previous pin, by the
// name the title scene's dispatcher had then (plans/M12 phase 3: script
// mods across decomp iterations; the mods gate runs it).

export default mod(({ game, hook, log }) => {
  let calls = 0;
  return {
    id: "old-names-js",
    version: "0.1.0",
    hooks: [
      hook(game.ovl_27_func_800B7EEC, {
        before() {
          if (++calls === 10) log("old name: 10 calls");
        },
      }),
    ],
  };
});
