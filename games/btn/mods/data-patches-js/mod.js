// @ts-check
// data-patches' script twin: a JS function stored over the executable's
// message window handler 3 (the game calls it through its own table, at a
// synthetic address), and a u16 in the title overlay's data, written as
// the overlay loads.

export default mod(({ game, log }) => {
  let opened = 0;
  return {
    id: "data-patches-js",
    version: "0.1.0",
    data: [
      {
        target: game.addr.gMessageWindowHandlers,
        offset: 12,
        type: "fnptr",
        value: () => {
          if (opened++ === 0) log("message window opened");
          return game.OpenMessageWindow();
        },
      },
      { target: game.addr.gOvl27SpriteFrameIds0, offset: 0, type: "u16", value: 64 },
    ],
  };
});
