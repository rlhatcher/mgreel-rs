<script lang="ts">
  // A small waveform of one splice for the splice list.
  type Props = { left: Float32Array; right: Float32Array; start: number; end: number };
  let { left, right, start, end }: Props = $props();

  const WIDTH = 72;
  const HEIGHT = 18;
  let canvas: HTMLCanvasElement;

  $effect(() => {
    const dpr = window.devicePixelRatio || 1;
    canvas.width = WIDTH * dpr;
    canvas.height = HEIGHT * dpr;
    const ctx = canvas.getContext("2d")!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = getComputedStyle(canvas).getPropertyValue("--wave").trim();
    const per = (end - start) / WIDTH;
    // Long splices are sampled, not scanned in full: 256 reads per column.
    const step = Math.max(1, Math.floor(per / 256));
    const mid = HEIGHT / 2;
    for (let x = 0; x < WIDTH; x++) {
      const a = Math.floor(start + x * per);
      const b = Math.min(end, Math.floor(start + (x + 1) * per));
      let peak = 0;
      for (let i = a; i < b; i += step) {
        peak = Math.max(peak, Math.abs(left[i]), Math.abs(right[i]));
      }
      const h = Math.max(1, Math.min(1, peak) * (mid - 1) * 2);
      ctx.fillRect(x, mid - h / 2, 1, h);
    }
  });
</script>

<canvas bind:this={canvas} style:width="{WIDTH}px" style:height="{HEIGHT}px"></canvas>

<style>
  canvas {
    display: block;
  }
</style>
