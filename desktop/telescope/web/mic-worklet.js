// The microphone as 48 kHz mono 16-bit samples, 20 ms at a time, whatever rate the browser runs its audio at.
class MicSender extends AudioWorkletProcessor {
  constructor() {
    super();
    this.step = sampleRate / 48000;  // input samples per output sample
    this.pos = 0;  // where the next output sample falls, counted from the start of the next input block
    this.prev = 0;  // the last sample of the previous block, for interpolating across the boundary
    this.out = new Int16Array(960);
    this.n = 0;
  }

  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (!ch || !ch.length) return true;
    let pos = this.pos;
    while (pos < ch.length - 1) {
      const i = Math.floor(pos);
      const frac = pos - i;
      const a = i < 0 ? this.prev : ch[i];
      const b = ch[i + 1];
      const s = Math.max(-1, Math.min(1, a + (b - a) * frac));
      this.out[this.n++] = s < 0 ? s * 32768 : s * 32767;
      if (this.n === this.out.length) {
        this.port.postMessage(this.out.buffer, [this.out.buffer]);
        this.out = new Int16Array(960);
        this.n = 0;
      }
      pos += this.step;
    }
    this.pos = pos - ch.length;
    this.prev = ch[ch.length - 1];
    return true;
  }
}

registerProcessor("mic-sender", MicSender);
