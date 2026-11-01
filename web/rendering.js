// Host-only styles and their controls. Adding another style means registering
// its options here and its factory in the WebRenderer binding, not guest code.
const STYLES = [
  { id: "native", wire: 0, name: "Original", note: "Native PS1 pixels, without visual enhancements.", options: [] },
  {
    id: "clean-polygons", wire: 1, name: "Clean Polygons",
    note: "Sharper polygon edges and subpixel colour/UV interpolation. Game-visible VRAM stays native.",
    options: [
      { key: "scale", id: "resolution-scale", name: "Internal resolution", initial: 2,
        choices: [[1, "1×"], [2, "2×"], [4, "4×"]] },
      { key: "textures", id: "texture-filter", name: "Texture sampling", initial: 0,
        choices: [[0, "Nearest"], [1, "Smooth 3D"]],
        note: "Smooth filters polygon textures; rectangles stay crisp. UI drawn with polygons is filtered too." },
      { key: "dithering", id: "colour-dithering", name: "Dithering", initial: 0,
        choices: [[0, "Original"], [1, "Off"]],
        note: "Off removes renderer-generated stippling, not patterns baked into texture artwork." },
      { key: "precision", id: "colour-precision", name: "Colour precision", initial: 0,
        choices: [[0, "Native (15-bit)"], [1, "High (24-bit)"]],
        note: "High keeps 8 bits per channel through visual shading, filtering and blending. Try it with dithering off." },
    ],
  },
];
const STORAGE_KEY = "psxw.rendering";
export const NATIVE_RENDER_SETTINGS = 1;

// One atomic word publishes a complete configuration, never a mixture of two
// edits. These are host settings, not renderer commands or a guest ABI.
export function unpackRenderSettings(word) {
  return { style: (word >>> 8) & 255, scale: word & 255, textures: (word >>> 16) & 255,
    dithering: (word >>> 24) & 1, precision: (word >>> 25) & 1 };
}

export function createRenderingPanel(panel, params) {
  const buffer = new SharedArrayBuffer(4);
  const shared = new Int32Array(buffer);
  const choices = Object.fromEntries(STYLES.map((s) => [s.id, Object.fromEntries(s.options.map((o) => [o.key, o.initial]))]));
  let selected = "native";
  try {
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY));
    if (saved && STYLES.some((s) => s.id === saved.renderer)) {
      selected = saved.renderer;
      for (const style of STYLES) {
        for (const option of style.options) {
          const value = saved.options?.[style.id]?.[option.key];
          if (option.choices.some(([v]) => v === value)) choices[style.id][option.key] = value;
        }
      }
    } else {
      // Keep the old single-dropdown preference when upgrading an existing tab.
      const scale = Number(localStorage.getItem("psxw.resolutionScale"));
      if ([2, 4].includes(scale)) {
        selected = "clean-polygons";
        choices[selected].scale = scale;
      }
    }
  } catch {}
  const scale = Number(params.get("scale"));
  if (params.has("scale") && [1, 2, 4].includes(scale)) {
    choices["clean-polygons"].scale = scale;
    selected = scale === 1 ? "native" : "clean-polygons";
  }
  const textures = params.get("textures");
  if (["nearest", "smooth"].includes(textures)) {
    choices["clean-polygons"].textures = textures === "smooth" ? 1 : 0;
    if (textures === "smooth") selected = "clean-polygons";
  }
  for (const [key, values] of [["dithering", ["original", "off"]], ["precision", ["native", "high"]]]) {
    const value = values.indexOf(params.get(key));
    if (value >= 0) {
      choices["clean-polygons"][key] = value;
      if (value === 1) selected = "clean-polygons";
    }
  }
  if (STYLES.some((s) => s.id === params.get("renderer"))) selected = params.get("renderer");

  const heading = document.createElement("h2");
  heading.textContent = "Rendering";
  const label = document.createElement("label");
  label.textContent = "Visual renderer ";
  const selector = document.createElement("select");
  selector.id = "visual-renderer";
  for (const style of STYLES) selector.add(new Option(style.name, style.id));
  label.append(selector);
  const note = document.createElement("p");
  note.id = "rendering-note";
  const options = document.createElement("div");
  options.className = "render-options";
  panel.replaceChildren(heading, label, note, options);

  function persist() {
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify({ renderer: selected, options: choices })); } catch {}
  }
  function publish() {
    const style = STYLES.find((s) => s.id === selected);
    const config = choices[selected];
    Atomics.store(shared, 0, (config.scale || 1) | (style.wire << 8) | ((config.textures || 0) << 16)
      | ((config.dithering || 0) << 24) | ((config.precision || 0) << 25));
    persist();
  }
  function render() {
    selector.value = selected;
    const style = STYLES.find((s) => s.id === selected);
    note.textContent = style.note;
    options.replaceChildren();
    // Inactive styles remain configurable, so a rejected 4× selection does not
    // hide the controls needed to lower it before retrying.
    for (const configurable of STYLES.filter((s) => s.options.length)) {
      const group = document.createElement("fieldset");
      const legend = document.createElement("legend");
      legend.textContent = `${configurable.name} options${configurable.id === selected ? "" : " (inactive)"}`;
      group.append(legend);
      for (const option of configurable.options) {
        const field = document.createElement("label");
        const title = document.createElement("span");
        title.textContent = option.name;
        const control = document.createElement("select");
        control.id = option.id;
        for (const [value, name] of option.choices) control.add(new Option(name, String(value)));
        control.value = String(choices[configurable.id][option.key]);
        control.addEventListener("change", () => {
          choices[configurable.id][option.key] = Number(control.value);
          publish();
        });
        field.append(title, control);
        if (option.note) {
          const help = document.createElement("small");
          help.textContent = option.note;
          field.append(help);
        }
        group.append(field);
      }
      options.append(group);
    }
  }
  selector.addEventListener("change", () => {
    selected = selector.value;
    render();
    publish();
  });
  render();
  publish();
  return {
    buffer,
    accept(word) {
      // A late acknowledgement must not overwrite a more recent user choice.
      if (word !== Atomics.load(shared, 0)) return;
      const config = unpackRenderSettings(word);
      const style = STYLES.find((s) => s.wire === config.style);
      if (!style) return;
      const changedStyle = selected !== style.id;
      selected = style.id;
      if (style.options.length) Object.assign(choices[selected], {
        scale: config.scale, textures: config.textures, dithering: config.dithering, precision: config.precision,
      });
      if (changedStyle) render();
      else for (const option of style.options) document.getElementById(option.id).value = String(choices[selected][option.key]);
      persist();
    },
  };
}
