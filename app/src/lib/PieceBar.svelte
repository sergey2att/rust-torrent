<script lang="ts">
  // Прогресс-бар в стиле Transmission: бар рисуется по кускам — скачанные
  // куски сплошные, «дыры» в распределении видны (редкие куски = разрывы).
  // Данные: упакованные 2-битные состояния из engine (0 нет, 1 качается,
  // 2 готов). Пустой массив — обычная заливка по проценту.
  let {
    pieces,
    totalPieces,
    percent,
    doneColor,
  }: { pieces: number[]; totalPieces: number; percent: number; doneColor: string } = $props();

  let canvas: HTMLCanvasElement;
  let wrapper: HTMLDivElement;

  const MISSING = "#2b2b2c";

  function draw() {
    if (!canvas || !wrapper) return;
    const dpr = window.devicePixelRatio || 1;
    const w = wrapper.clientWidth;
    const h = wrapper.clientHeight;
    if (w === 0) return;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(dpr, dpr);

    // Фон — отсутствующие куски.
    ctx.fillStyle = MISSING;
    ctx.fillRect(0, 0, w, h);

    if (pieces.length === 0 || totalPieces === 0) {
      // Нет per-piece данных (метаданные/речек) — простая заливка.
      if (percent > 0) {
        ctx.fillStyle = doneColor;
        ctx.globalAlpha = 0.9;
        ctx.fillRect(0, 0, (w * percent) / 100, h);
        ctx.globalAlpha = 1;
      }
      return;
    }

    // Кусок в работе — считаем наполовину: светится, но не как готовый.
    const value = (i: number): number => {
      const code = (pieces[i >> 2] >> ((i & 3) * 2)) & 3;
      return code === 2 ? 1 : code === 1 ? 0.5 : 0;
    };

    ctx.fillStyle = doneColor;
    const pxPerPiece = w / totalPieces;
    if (pxPerPiece >= 1) {
      // Кусков меньше, чем пикселей: слот на кусок.
      for (let i = 0; i < totalPieces; i++) {
        const v = value(i);
        if (v > 0) ctx.fillRect(i * pxPerPiece, 0, Math.max(pxPerPiece * v, 0.5), h);
      }
    } else {
      // Кусков больше, чем пикселей: на пиксель — среднее по его кускам
      // (доля заполнения); готовые пиксели — целиком, частичные — с альфой.
      for (let x = 0; x < w; x++) {
        const from = Math.floor((x * totalPieces) / w);
        const to = Math.max(Math.floor(((x + 1) * totalPieces) / w), from + 1);
        let sum = 0;
        for (let i = from; i < to; i++) sum += value(i);
        const coverage = sum / (to - from);
        if (coverage > 0) {
          ctx.globalAlpha = coverage;
          ctx.fillRect(x, 0, 1, h);
        }
      }
      ctx.globalAlpha = 1;
    }
  }

  $effect(() => {
    void pieces;
    void percent;
    void doneColor;
    draw();
  });

  $effect(() => {
    const ro = new ResizeObserver(() => draw());
    ro.observe(wrapper);
    return () => ro.disconnect();
  });
</script>

<div class="wrap" bind:this={wrapper}>
  <canvas bind:this={canvas}></canvas>
</div>

<style>
  .wrap {
    flex: 1;
    height: 6px;
    border-radius: 3px;
    overflow: hidden;
  }

  canvas {
    display: block;
    width: 100%;
    height: 100%;
  }
</style>
