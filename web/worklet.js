// AudioWorklet: downsample mic audio from the context's native rate (usually
// 44.1k or 48k) to 24 kHz mono PCM16 and post it back to the main thread.
// Chunk size ≈ 40 ms at 24 kHz → ~960 samples → 1920 bytes.
class PcmDownsampler extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.targetRate = 24000;
    this.ratio = sampleRate / this.targetRate;   // e.g. 48000/24000 = 2
    this.buffer = [];                             // accumulate resampled float samples
    this.chunkSize = 960;                         // 40 ms @ 24 kHz
    this.pos = 0;                                 // fractional read position in input
  }

  process(inputs) {
    const input = inputs[0];
    if (!input || !input[0]) return true;
    const ch0 = input[0];

    // Linear-interpolation resample to 24 kHz.
    while (this.pos < ch0.length) {
      const i = Math.floor(this.pos);
      const frac = this.pos - i;
      const s = ch0[i] * (1 - frac) + (ch0[i + 1] ?? ch0[i]) * frac;
      this.buffer.push(s);
      this.pos += this.ratio;
    }
    this.pos -= ch0.length;

    while (this.buffer.length >= this.chunkSize) {
      const chunk = this.buffer.splice(0, this.chunkSize);
      const pcm = new Int16Array(this.chunkSize);
      for (let i = 0; i < this.chunkSize; i++) {
        const v = Math.max(-1, Math.min(1, chunk[i]));
        pcm[i] = v < 0 ? v * 0x8000 : v * 0x7fff;
      }
      this.port.postMessage(pcm.buffer, [pcm.buffer]);
    }
    return true;
  }
}

registerProcessor('pcm-downsampler', PcmDownsampler);
