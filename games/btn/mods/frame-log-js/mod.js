// @ts-check
// frame-log's script twin: an `after` hook on the manifest's `frame`
// point, counting frames and logging every hundredth.

export default mod(({ game, hook, log }) => {
  let frames = 0;
  return {
    id: "frame-log-js",
    version: "0.1.0",
    hooks: [
      hook(game.points.frame, {
        after() {
          frames++;
          if (frames % 100 === 0) log(`frames ${frames}`);
        },
      }),
    ],
  };
});
