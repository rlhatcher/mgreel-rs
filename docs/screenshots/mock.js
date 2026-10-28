// A stand-in for the Tauri runtime, so the app's pages render in a browser
// with a made-up card for the documentation's screenshots. capture.py loads
// it before the app starts.
(() => {
  const SR = 48000;

  // A repeatable random sequence, so every run draws the same waveforms.
  function random(seed) {
    let s = seed >>> 0;
    return () => {
      s = (s * 1664525 + 1013904223) >>> 0;
      return s / 2 ** 32;
    };
  }

  const reels = [
    { name: "Gulls at the pier", note: "dawn, low tide", seconds: 64, splices: 14 },
    { name: "Rain on a tin roof", note: "", seconds: 31, splices: 6 },
    // The reel the editor opens: reelBuffer() below.
    { name: "Ferry horn", note: "two blasts, harbour echo", seconds: 12, splices: 9 },
    { name: "Wind in wires", note: "", seconds: 88, splices: 9 },
    { name: "Gravel footsteps", note: "slow, then running", seconds: 47, splices: 18 },
    { name: "Kettle", note: "", seconds: 22, splices: 4 },
    null,
    { name: "Radio between stations", note: "shortwave", seconds: 150, splices: 30 },
    { name: "Church bells", note: "", seconds: 96, splices: 12 },
    { name: "", note: "", seconds: 40, splices: 1 },
    { name: "Market crowd", note: "Saturday", seconds: 120, splices: 16 },
  ];

  const EDITED = 2;
  const EDITED_MARKERS = [52000, 118000, 171000, 236000, 300000, 365000, 420000, 497000];

  const slots = Array.from({ length: 32 }, (_, i) => {
    const reel = reels[i] ?? null;
    const frames = reel ? reel.seconds * SR : 0;
    const count = reel ? reel.splices - 1 : 0;
    const rand = random(i + 1);
    let at = 0;
    const markers = [];
    for (let k = 0; k < count && i !== EDITED; k++) {
      at += (frames / (count + 1)) * (0.6 + 0.8 * rand());
      if (at < frames) markers.push(Math.round(at));
    }
    if (i === EDITED) markers.push(...EDITED_MARKERS);
    return {
      index: i,
      fileName: i < 9 ? `mg${i + 1}.wav` : `mg${String.fromCharCode(97 + i - 9)}.wav`,
      present: !!reel,
      frames,
      seconds: frames / SR,
      markers,
      formatOk: true,
      convertible: false,
      problems: [],
      name: reel?.name ?? "",
      note: reel?.note ?? "",
    };
  });

  // Field-recording-like audio: noise under bursts that swell and fade.
  function reelBuffer() {
    const n = SR * 12;
    const markers = EDITED_MARKERS;
    const buf = new ArrayBuffer(12 + markers.length * 4 + n * 8);
    const v = new DataView(buf);
    v.setUint32(0, markers.length, true);
    markers.forEach((m, i) => v.setUint32(12 + i * 4, m, true));
    const off = 12 + markers.length * 4;
    const l = new Float32Array(buf, off, n);
    const r = new Float32Array(buf, off + n * 4, n);
    const rand = random(7);
    const starts = [0, ...markers];
    let lp = 0;
    for (let i = 0; i < n; i++) {
      // Each splice is one event: a quick rise, then a slow decay.
      let s = starts.length - 1;
      while (s > 0 && starts[s] > i) s--;
      const t = (i - starts[s]) / SR;
      const env = Math.min(1, t * 12) * Math.exp(-t * (1.2 + (s % 3) * 0.7));
      // Low-passed noise, a little different per splice.
      lp += (rand() * 2 - 1 - lp) * (0.08 + (s % 4) * 0.06);
      const floor = 0.015 * (rand() * 2 - 1);
      l[i] = floor + 0.85 * env * lp * 2.4;
      r[i] = floor + 0.85 * env * lp * 2.2;
    }
    return buf;
  }

  const options = {
    exists: true,
    firmware: 203,
    text: "",
    entries: [
      { key: "vsop", value: "0", comment: "Varispeed option", name: "Vari-Speed", ratio: false,
        choices: [{ value: "0", label: "bidirectional classic" }, { value: "1", label: "bidirectional 1 V/oct" }, { value: "2", label: "positive only, 1 V/oct" }] },
      { key: "inop", value: "0", comment: "Input option", name: "Input", ratio: false,
        choices: [{ value: "0", label: "record S.O.S. mix" }, { value: "1", label: "record input only" }] },
      { key: "omod", value: "1", comment: "Organize option", name: "Organize", ratio: false,
        choices: [{ value: "0", label: "organize at end of gene" }, { value: "1", label: "organize immediately" }] },
      { key: "mcr1", value: "2.00000", comment: "Morph chord ratio 1", name: "Morph chord ratio 1", ratio: true, choices: null },
    ],
  };

  const day = 864e5;
  const sets = [
    { name: "Coast walk", dir: "/Users/you/Music/Morphagene Sets/Coast walk", reels: [0, 1, 2, 3, 4, 5, 7, 8], names: ["Gulls at the pier", "Rain on a tin roof", "Ferry horn", "Wind in wires", "Gravel footsteps"], hasOptions: true, modified: Date.now() - day },
    { name: "Kitchen percussion", dir: "/Users/you/Music/Morphagene Sets/Kitchen percussion", reels: [0, 1, 2, 3], names: ["Kettle", "Pans", "Drawer"], hasOptions: true, modified: Date.now() - 6 * day },
    { name: "Shortwave", dir: "/Users/you/Music/Morphagene Sets/Shortwave", reels: [0, 1, 2], names: [], hasOptions: false, modified: Date.now() - 20 * day },
  ];

  const sources = [
    { path: "/x/harbour 01.wav", name: "harbour 01.wav", frames: SR * 41, markers: [SR * 6, SR * 14, SR * 22, SR * 31], error: null },
    { path: "/x/harbour 02.wav", name: "harbour 02.wav", frames: SR * 27, markers: [], error: null },
    { path: "/x/bell, close.wav", name: "bell, close.wav", frames: SR * 9, markers: [], error: null },
  ];

  let next = 1;
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
    transformCallback: () => next++,
    convertFileSrc: (p) => p,
    async invoke(cmd) {
      switch (cmd) {
        case "list_cards": return [{ root: "/Volumes/MORPHAGENE", label: "MORPHAGENE" }];
        case "list_slots": return slots;
        case "check_card": return { wrongFilesystem: null, stray: [], clutter: 0, namesError: null };
        case "list_sets": return sets;
        case "read_options": return options;
        case "load_reel": return reelBuffer();
        case "probe_sources": return sources;
        case "plugin:dialog|open": return window.nextOpen ?? null;
        default: return 0;
      }
    },
  };
})();
