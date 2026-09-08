<script lang="ts">
  import type { TorrentStatus, TorrentState } from "../lib/types";
  import { formatBytes, formatSpeed, formatEta, progressPercent } from "../lib/format";
  import PieceBar from "./PieceBar.svelte";

  interface Props {
    torrent: TorrentStatus;
    onPause: (handle: string) => void;
    onResume: (handle: string) => void;
    onRemove: (torrent: TorrentStatus) => void;
  }

  let { torrent, onPause, onResume, onRemove }: Props = $props();

  const stateMeta: Record<string, string> = {
    FetchingMetadata: "Метаданные",
    Downloading: "Скачивание",
    Seeding: "Раздача",
    Paused: "Пауза",
  };

  let state = $derived(torrent.state);
  let percent = $derived(progressPercent(torrent.completed_pieces, torrent.total_pieces));
  let isPaused = $derived(state === "Paused");
  let isActive = $derived(
    !isPaused && !(typeof state === "object" && "Error" in state),
  );
  let done = $derived(state === "Seeding" || percent === 100);
  // Canvas не понимает CSS-переменные — литералы из app.css.
  let doneColor = $derived(done ? "#5bc87c" : "#6a80ff");
  // Речек: бар показывает процент ПРОВЕРКИ (жёлтый), не загрузки.
  let recheck = $derived(
    typeof state === "object" && "Rechecking" in state ? state.Rechecking : null,
  );
  let recheckPct = $derived(recheck ? progressPercent(recheck.done, recheck.total) : 0);
  let barPct = $derived(recheck ? recheckPct : percent);
  let barColor = $derived(recheck ? "#e0b64f" : doneColor);
  let barLabel = $derived.by(() => {
    if (recheck) return `Проверка ${recheckPct}%`;
    if (typeof state === "object" && "Error" in state) return `Ошибка: ${state.Error}`;
    if (state === "FetchingMetadata") return "Метаданные…";
    return `${stateMeta[state as string] ?? "?"} ${percent}%`;
  });
  let etaLabel = $derived(
    torrent.eta_seconds === null || done ? "—" : formatEta(torrent.eta_seconds),
  );
  let sizeLabel = $derived(torrent.total_bytes > 0 ? formatBytes(torrent.total_bytes) : "—");
  // Тоталы — в tooltip имени: колонок на них жалко.
  let title = $derived.by(() => {
    const base = torrent.name ?? torrent.handle;
    if (torrent.downloaded_bytes === 0 && torrent.uploaded_bytes === 0) return base;
    return `${base} · скачано ${formatBytes(torrent.downloaded_bytes)} · роздано ${formatBytes(torrent.uploaded_bytes)}`;
  });
</script>

<div class="row" class:dim={isPaused}>
  <div class="cell name-cell">
    <div class="name" {title}>{torrent.name ?? "Получение метаданных…"}</div>
  </div>
  <div class="cell num size" title={sizeLabel}>{sizeLabel}</div>
  <div class="cell progress-cell">
    <PieceBar
      pieces={torrent.pieces}
      totalPieces={torrent.total_pieces}
      percent={barPct}
      doneColor={barColor}
      label={barLabel}
    />
  </div>
  <!-- Скорость 0 — серым, но элемент на месте: никакой перестановки строки. -->
  <div class="cell num" class:off={torrent.download_speed_bps === 0}>
    ↓ {formatSpeed(torrent.download_speed_bps)}
  </div>
  <div class="cell num" class:off={torrent.upload_speed_bps === 0}>
    ↑ {formatSpeed(torrent.upload_speed_bps)}
  </div>
  <div class="cell num dim-col">{etaLabel}</div>
  <div class="cell num dim-col">{torrent.connected_peers > 0 ? torrent.connected_peers : "—"}</div>
  <div class="cell actions">
    {#if isPaused}
      <button class="ghost" title="Возобновить" onclick={() => onResume(torrent.handle)}>▶</button>
    {:else}
      <button class="ghost" title="Пауза" disabled={!isActive} onclick={() => onPause(torrent.handle)}>⏸</button>
    {/if}
    <button class="ghost danger" title="Удалить" onclick={() => onRemove(torrent)}>✕</button>
  </div>
</div>

<style>
  .row {
    display: grid;
    grid-template-columns: var(--row-grid);
    column-gap: 14px;
    align-items: center;
    min-height: 58px;
    padding: 7px 16px;
    transition: background 0.12s;
  }

  .row:hover {
    background: var(--bg-hover);
  }

  .row.dim {
    opacity: 0.55;
  }

  .cell {
    min-width: 0;
  }

  .name {
    font-weight: 700;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .progress-cell {
    display: flex;
    align-items: center;
  }

  .num {
    text-align: right;
    font-size: 13px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .num:not(.off):not(.dim-col) {
    color: var(--text);
  }

  .dim-col {
    color: var(--text-dim);
  }

  .off {
    color: var(--text-faint);
  }

  .actions {
    display: flex;
    gap: 4px;
    justify-content: flex-end;
  }

  .ghost {
    border: none;
    padding: 4px 6px;
    color: var(--text-dim);
  }

  .ghost:hover:not(:disabled) {
    color: var(--text);
  }

  .danger:hover:not(:disabled) {
    color: var(--red);
  }
</style>
