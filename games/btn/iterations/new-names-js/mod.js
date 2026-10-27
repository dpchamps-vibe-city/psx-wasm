// @ts-check
// A script mod written against btn's decomp at its current pin, by the
// name the title scene's dispatcher has now (plans/M12 phase 3: script
// mods across decomp iterations; the mods gate runs it).

export default mod(({ game, hook, log }) => {
  let calls = 0;
  return {
    id: "new-names-js",
    version: "0.1.0",
    hooks: [
      hook(game.Ovl27DispatchTitleScenePhase, {
        before() {
          if (++calls === 10) log("new name: 10 calls");
        },
      }),
    ],
  };
});
