// The AudioWorklet (M5): plays the SharedArrayBuffer ring the game's audio
// will fill (M6). Until then the ring stays empty and the output is silence.

class PsxwAudio extends AudioWorkletProcessor {
  constructor(opts) {
    super();
    const ring = opts.processorOptions.ring;
    this.head = new Int32Array(ring, 0, 2); // [write index, read index] in frames
    this.samples = new Int16Array(ring, 8); // interleaved L, R
  }

  process(_inputs, outputs) {
    const [left, right] = outputs[0];
    const frames = this.samples.length / 2;
    let read = Atomics.load(this.head, 1);
    const write = Atomics.load(this.head, 0);
    for (let i = 0; i < left.length; i++) {
      if (read !== write) {
        left[i] = this.samples[2 * read] / 32768;
        right[i] = this.samples[2 * read + 1] / 32768;
        read = (read + 1) % frames;
      } else {
        left[i] = right[i] = 0;
      }
    }
    Atomics.store(this.head, 1, read);
    return true;
  }
}

registerProcessor("psxw-audio", PsxwAudio);
