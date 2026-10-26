<script lang="ts">
  // Canvas waveform with splice markers. Click sets the cursor, drag a
  // marker to move it, double-click to add one. Locked markers don't move.
  // Pinch or Cmd-scroll zooms, horizontal scroll pans.
  import { onMount, untrack } from "svelte";
  import { spectrogram } from "./spectrum";

  type Props = {
    left: Float32Array;
    right: Float32Array;
    markers: number[];
    cursor: number;
    playhead: number | null;
    selectedMarker: number | null;
    /** Frames of markers that can't be moved. */
    locked: ReadonlySet<number>;
    /** Draw a spectrogram instead of the waveform. */
    spectrum: boolean;
    onseek?: (frame: number) => void;
    /** Double-click asks for a marker here; the parent adds it. */
    onadd: (frame: number) => void;
    /** A drag is about to move marker `index`. */
    ondragstart: (index: number) => void;
    /** A drag moved marker `index` and has ended. */
    ondragend: (index: number) => void;
  };
  let {
    left,
    right,
    markers = $bindable(),
    cursor = $bindable(),
    playhead,
    selectedMarker = $bindable(),
    locked,
    spectrum,
    onseek,
    onadd,
    ondragstart,
    ondragend,
  }: Props = $props();

  const BLOCK = 256;
  const HANDLE_PX = 6;
  const HEIGHT = 240;

  let canvas: HTMLCanvasElement;
  let width = $state(800);
  let viewStart = $state(0);
  let viewEnd = $state(1);
  let dragging: number | null = null;
  let moved = false;
  let hoverMarker = $state(false);

  const frames = $derived(left.length);

  // Min and max of both channels per block of 256 frames, so a zoomed-out
  // redraw reads ~1/256th of the samples.
  const blocks = $derived.by(() => {
    const n = Math.ceil(frames / BLOCK);
    const min = new Float32Array(n);
    const max = new Float32Array(n);
    for (let b = 0; b < n; b++) {
      let lo = 0;
      let hi = 0;
      const end = Math.min(frames, (b + 1) * BLOCK);
      for (let i = b * BLOCK; i < end; i++) {
        const l = left[i];
        const r = right[i];
        if (l < lo) lo = l;
        if (r < lo) lo = r;
        if (l > hi) hi = l;
        if (r > hi) hi = r;
      }
      min[b] = lo;
      max[b] = hi;
    }
    return { min, max };
  });

  $effect(() => {
    // Show the whole reel at first. After an edit changes the length, keep
    // the view where it was, pulled back inside the reel.
    const f = Math.max(1, frames);
    untrack(() => {
      const span = viewEnd - viewStart;
      if (viewEnd <= 1 || span >= f) {
        viewStart = 0;
        viewEnd = f;
      } else if (viewEnd > f) {
        viewStart = f - span;
        viewEnd = f;
      }
    });
  });

  const toFrame = (x: number) => Math.round(viewStart + (x / width) * (viewEnd - viewStart));
  const toX = (frame: number) => ((frame - viewStart) / (viewEnd - viewStart)) * width;

  function markerAt(x: number): number | null {
    let best: number | null = null;
    let bestDist = HANDLE_PX;
    markers.forEach((m, i) => {
      const d = Math.abs(toX(m) - x);
      if (d <= bestDist) {
        best = i;
        bestDist = d;
      }
    });
    return best;
  }

  function columnRange(f0: number, f1: number): [number, number] {
    let lo = 0;
    let hi = 0;
    if (f1 - f0 >= BLOCK) {
      const b1 = Math.min(blocks.min.length, Math.ceil(f1 / BLOCK));
      for (let b = Math.floor(f0 / BLOCK); b < b1; b++) {
        if (blocks.min[b] < lo) lo = blocks.min[b];
        if (blocks.max[b] > hi) hi = blocks.max[b];
      }
    } else {
      const end = Math.min(frames, Math.max(Math.floor(f0) + 1, Math.ceil(f1)));
      for (let i = Math.floor(f0); i < end; i++) {
        lo = Math.min(lo, left[i], right[i]);
        hi = Math.max(hi, left[i], right[i]);
      }
    }
    return [lo, hi];
  }

  /** Spectrogram rows. Columns are every second pixel; drawing scales them up. */
  const SPEC_ROWS = 160;
  let specCache: { key: string; left: Float32Array; image: HTMLCanvasElement } | null = null;

  /** A CSS color as RGB numbers. */
  function rgb(color: string): [number, number, number] {
    const c = document.createElement("canvas").getContext("2d")!;
    c.fillStyle = color;
    const v = c.fillStyle as string;
    if (v.startsWith("#")) return [1, 3, 5].map((i) => parseInt(v.slice(i, i + 2), 16)) as [number, number, number];
    const n = v.match(/[\d.]+/g)!.map(Number);
    return [n[0], n[1], n[2]];
  }

  function spectrumImage(color: (name: string) => string) {
    const columns = Math.max(1, Math.ceil(width / 2));
    const stops = [rgb(color("--panel")), rgb(color("--accent")), rgb(color("--text"))];
    const key = `${viewStart}:${viewEnd}:${columns}:${stops.join()}`;
    if (specCache && specCache.key === key && specCache.left === left) return specCache.image;
    const levels = spectrogram(left, right, viewStart, viewEnd, columns, SPEC_ROWS);
    const image = document.createElement("canvas");
    image.width = columns;
    image.height = SPEC_ROWS;
    const ictx = image.getContext("2d")!;
    const data = ictx.createImageData(columns, SPEC_ROWS);
    for (let c = 0; c < columns; c++) {
      for (let r = 0; r < SPEC_ROWS; r++) {
        const v = levels[c * SPEC_ROWS + r];
        // Background to accent over the lower 70%, then accent to text.
        const [a, b, t] = v < 0.7 ? [stops[0], stops[1], v / 0.7] : [stops[1], stops[2], (v - 0.7) / 0.3];
        const i = ((SPEC_ROWS - 1 - r) * columns + c) * 4;
        for (let k = 0; k < 3; k++) data.data[i + k] = a[k] + (b[k] - a[k]) * t;
        data.data[i + 3] = 255;
      }
    }
    ictx.putImageData(data, 0, 0);
    specCache = { key, left, image };
    return image;
  }

  function draw() {
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = width * dpr;
    canvas.height = HEIGHT * dpr;
    const ctx = canvas.getContext("2d")!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const css = getComputedStyle(canvas);
    const color = (name: string) => css.getPropertyValue(name).trim();
    const mid = HEIGHT / 2;

    ctx.fillStyle = color("--panel");
    ctx.fillRect(0, 0, width, HEIGHT);

    if (spectrum) ctx.drawImage(spectrumImage(color), 0, 0, width, HEIGHT);

    // Shade alternate splices and highlight the one under the cursor. Over
    // a spectrogram only the highlight shows, as a tint.
    const starts = [0, ...markers];
    starts.forEach((s, i) => {
      const e = starts[i + 1] ?? frames;
      const x0 = Math.max(0, toX(s));
      const x1 = Math.min(width, toX(e));
      if (x1 <= 0 || x0 >= width) return;
      if (cursor >= s && cursor < e) {
        ctx.fillStyle = color("--select");
        ctx.fillRect(x0, 0, x1 - x0, HEIGHT);
      } else if (i % 2 === 1 && !spectrum) {
        ctx.fillStyle = color("--panel-2");
        ctx.fillRect(x0, 0, x1 - x0, HEIGHT);
      }
    });

    if (!spectrum) {
      ctx.fillStyle = color("--wave");
      const fpp = (viewEnd - viewStart) / width;
      for (let x = 0; x < width; x++) {
        const f0 = viewStart + x * fpp;
        if (f0 >= frames) break;
        const [lo, hi] = columnRange(f0, f0 + fpp);
        const top = mid - hi * (mid - 4);
        const bottom = mid - lo * (mid - 4);
        ctx.fillRect(x, top, 1, Math.max(1, bottom - top));
      }
      ctx.fillStyle = color("--wave-dim");
      ctx.fillRect(0, mid, width, 0.5);
    }

    markers.forEach((m, i) => {
      const x = Math.round(toX(m)) + 0.5;
      if (x < -HANDLE_PX || x > width + HANDLE_PX) return;
      const isLocked = locked.has(m);
      ctx.strokeStyle = ctx.fillStyle = color(isLocked ? "--marker-locked" : "--marker");
      ctx.lineWidth = i === selectedMarker ? 2 : 1;
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, HEIGHT);
      ctx.stroke();
      if (isLocked) {
        // A square handle for a locked marker, a triangle otherwise.
        ctx.fillRect(x - HANDLE_PX + 1, 0, 2 * HANDLE_PX - 2, HANDLE_PX + 1);
      } else {
        ctx.beginPath();
        ctx.moveTo(x - HANDLE_PX, 0);
        ctx.lineTo(x + HANDLE_PX, 0);
        ctx.lineTo(x, HANDLE_PX + 3);
        ctx.fill();
      }
    });

    ctx.fillStyle = color("--muted");
    ctx.fillRect(Math.round(toX(cursor)), 0, 1, HEIGHT);
    if (playhead !== null) {
      ctx.fillStyle = color("--text");
      ctx.fillRect(Math.round(toX(playhead)), 0, 2, HEIGHT);
    }
  }

  let frame = 0;
  $effect(() => {
    // Track everything draw() reads.
    void [width, viewStart, viewEnd, markers.length, ...markers, cursor, playhead, selectedMarker, blocks, locked, spectrum];
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(draw);
  });

  onMount(() => {
    const resize = new ResizeObserver(([entry]) => (width = Math.max(100, Math.floor(entry.contentRect.width))));
    resize.observe(canvas.parentElement!);
    const scheme = matchMedia("(prefers-color-scheme: dark)");
    const redraw = () => requestAnimationFrame(draw);
    scheme.addEventListener("change", redraw);
    return () => {
      resize.disconnect();
      scheme.removeEventListener("change", redraw);
    };
  });

  function pointerX(e: PointerEvent | MouseEvent | WheelEvent) {
    return e.clientX - canvas.getBoundingClientRect().left;
  }

  function onPointerDown(e: PointerEvent) {
    const x = pointerX(e);
    const hit = markerAt(x);
    if (hit !== null) {
      selectedMarker = hit;
      if (!locked.has(markers[hit])) {
        dragging = hit;
        moved = false;
        canvas.setPointerCapture(e.pointerId);
      }
      return;
    }
    selectedMarker = null;
    cursor = Math.max(0, Math.min(frames - 1, toFrame(x)));
    onseek?.(cursor);
  }

  function onPointerMove(e: PointerEvent) {
    const x = pointerX(e);
    if (dragging === null) {
      hoverMarker = markerAt(x) !== null;
      return;
    }
    const lo = (markers[dragging - 1] ?? 0) + 1;
    const hi = (markers[dragging + 1] ?? frames) - 1;
    const at = Math.max(lo, Math.min(hi, toFrame(x)));
    if (at === markers[dragging]) return;
    if (!moved) ondragstart(dragging);
    moved = true;
    const next = markers.slice();
    next[dragging] = at;
    markers = next;
  }

  function onPointerUp(e: PointerEvent) {
    if (dragging !== null) {
      canvas.releasePointerCapture(e.pointerId);
      if (moved) ondragend(dragging);
    }
    dragging = null;
  }

  function onDoubleClick(e: MouseEvent) {
    const at = toFrame(pointerX(e));
    if (at > 0 && at < frames) onadd(at);
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const span = viewEnd - viewStart;
    if (e.ctrlKey || e.metaKey) {
      // Zoom around the pointer.
      const anchor = toFrame(pointerX(e));
      const factor = Math.exp(e.deltaY * 0.01);
      zoomTo(anchor, span * factor, (anchor - viewStart) / span);
    } else {
      const delta = (e.deltaX || (e.shiftKey ? e.deltaY : 0)) * (span / width);
      pan(delta);
    }
  }

  function pan(delta: number) {
    const span = viewEnd - viewStart;
    viewStart = Math.max(0, Math.min(frames - span, viewStart + delta));
    viewEnd = viewStart + span;
  }

  function zoomTo(anchor: number, span: number, at = 0.5) {
    span = Math.max(Math.min(width, frames), Math.min(frames, span));
    viewStart = Math.max(0, Math.min(frames - span, anchor - span * at));
    viewEnd = viewStart + span;
  }

  export function zoom(factor: number) {
    zoomTo(cursor, (viewEnd - viewStart) / factor, (cursor - viewStart) / (viewEnd - viewStart));
  }

  export function fit() {
    viewStart = 0;
    viewEnd = Math.max(1, frames);
  }

  /** Scroll so `frame` is visible. */
  export function reveal(frame: number) {
    const span = viewEnd - viewStart;
    if (frame < viewStart || frame > viewEnd) pan(frame - span / 2 - viewStart);
  }
</script>

<div class="wrap" style:height="{HEIGHT}px">
  <canvas
    bind:this={canvas}
    style:width="{width}px"
    style:height="{HEIGHT}px"
    style:cursor={hoverMarker ? "ew-resize" : "text"}
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    ondblclick={onDoubleClick}
    onwheel={onWheel}
  ></canvas>
</div>

<style>
  .wrap {
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
  }
  canvas {
    display: block;
    touch-action: none;
  }
</style>
