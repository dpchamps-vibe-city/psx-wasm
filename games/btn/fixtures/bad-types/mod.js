// @ts-check
// A type-check fixture (plans/M12 phase 2, exit criterion 3): checked
// against btn's game.d.ts, it has exactly three errors, each on a line
// marked `error:` (the mods gate counts them).

export default mod(({ game, hook }) => {
  const player = game.types.Ovl11PrimaryMotionObject.at(game.addr.gPrimaryObjectBase);
  return {
    id: "bad-types",
    version: "0.1.0",
    hooks: [
      hook(game.Rnad, { before() {} }), // error: a misspelled target
      hook(game.Rand, {
        replace(limit) { // error: a handler of the wrong result type
          return `seven ${limit}`;
        },
      }),
      hook(game.points.frame, {
        after() {
          player.nosuch = 1; // error: a field the record doesn't have
        },
      }),
    ],
  };
});
