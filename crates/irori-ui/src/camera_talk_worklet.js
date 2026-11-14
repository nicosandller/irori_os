// Turns the microphone into what a camera's speaker takes (camera_talk.js,
// `docs/specs/protocols.md` §5.4): 8000 samples a second, 16-bit, posted 20 ms (160 samples)
// at a time.
//
// The sound arrives already low-passed below 4 kHz (camera_talk.js), so picking samples out of
// it at the lower rate, between two neighbours, doesn't fold high sounds back in as noise.
class IroriTalk extends AudioWorkletProcessor {
  constructor() {
    super();
    // How far apart, in the browser's samples, two of the camera's are.
    this.step = sampleRate / 8000;
    // Where the next sample is, from the start of the next block. Below zero is between the
    // last block's final sample and this one's first.
    this.position = 0;
    this.last = 0;
    this.frame = new Int16Array(160);
    this.filled = 0;
  }

  process(inputs) {
    const input = inputs[0] && inputs[0][0];
    if (!input || input.length === 0) return true;
    let position = this.position;
    while (Math.floor(position) + 1 < input.length) {
      const index = Math.floor(position);
      const between = position - index;
      const before = index < 0 ? this.last : input[index];
      const after = input[index + 1];
      const value = before + (after - before) * between;
      const clamped = Math.max(-1, Math.min(1, value));
      this.frame[this.filled++] = clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff;
      if (this.filled === this.frame.length) {
        const full = this.frame;
        this.port.postMessage(full.buffer, [full.buffer]);
        this.frame = new Int16Array(160);
        this.filled = 0;
      }
      position += this.step;
    }
    this.position = position - input.length;
    this.last = input[input.length - 1];
    return true;
  }
}

registerProcessor('irori-talk', IroriTalk);
