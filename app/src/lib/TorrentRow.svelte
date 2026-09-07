<script lang="ts">
  import type { TorrentStatus, TorrentState } from "../lib/types";
  import { formatBytes, formatSpeed, formatEta, progressPercent, piecesLabel } from "../lib/format";
  import PieceBar from "./PieceBar.svelte";

  interface Props {
    torrent: TorrentStatus;
    onPause: (handle: string) => void;
    onResume: (handle: string) => void;
    onRemove: (torrent: TorrentStatus) => void;
  }

  let { torrent, onPause, onResume, onRemove }: Props = $props();

  const stateMeta: Record<string, { label: string; color: string }> = {
    FetchingMetadata: { label: "Метаданные", color: "var(--purple)" },
    Downloading: { label: "Скачивание", color: "var(--accent)" },
    Seeding: { label: "Раздача", color: "var(--green)" },
    Paused: { label: "Пауза", color: "var(--text-dim)" },
  };

  let state = $derived(torrent.state);
  let badge = $derived.by(() => {
    if (typeof state === "object") {
      if ("Rechecking" in state) {
        return { label: "Проверка", color: "var(--yellow)" };
      }
      if ("Error" in state) return { label: "Ошибка", color: "var(--red)" };
    }
    return stateMeta[state as string] ?? { label: "?", color: "var(--text-dim)" };
  });

  let errorText = $derived(
    typeof state === "object" && "Error" in state ? state.Error : "",
  );
  let percent = $derived(progressPercent(torrent.completed_pieces, torrent.total_pieces));
  let isPaused = $derived(state === "Paused");
  let isActive = $derived(
    !isPaused && !(typeof state === "object" && "Error" in state),
  );
  let done = $derived(state === "Seeding" || percent === 100);
  // Canvas не понимает CSS-переменные — литералы из app.css.
  let doneColor = $derived(done ? "#5bc87c" : "#6a80ff");
  let eta = $derived(
    torrent.eta_seconds === null || done ? "" : formatEta(torrent.eta_seconds),
  );
  // Вторая строка под названием: тоталы, куски, ETA/речек, пиры.
  let recheckInfo = $derived(
    typeof state === "object" && "Rechecking" in state
      ? `проверка ${state.Rechecking.done}/${state.Rechecking.total}`
      : "",
  );
  let title = $derived(
    [
      torrent.name ?? torrent.handle,
      piecesLabel(torrent.completed_pieces, torrent.total_pieces) !== ""
        ? `${piecesLabel(torrent.completed_pieces, torrent.total_pieces)} кусков`
        : "",
    ]
      .filter(Boolean)
      .join(" · "),
  );
</script>

<div class="row" class:dim={isPaused}>
  <div class="cell name-cell">
    <div class="name" {title}>{torrent.name ?? "Получение метаданных…"}</div>
    <div class="sub">
      {#if recheckInfo}
        {recheckInfo}
      {:else if piecesLabel(torrent.completed_pieces, torrent.total_pieces) !== ""}
        {torrent.completed_pieces}/{torrent.total_pieces} кусков
      {/if}
      {#if eta}
        {recheckInfo || piecesLabel(torrent.completed_pieces, torrent.total_pieces) !== "" ? "· " : ""}осталось {eta}
      {/if}
      {#if torrent.connected_peers > 0}
        · пиров {torrent.connected_peers}
      {/if}
      {#if !recheckInfo && !eta && piecesLabel(torrent.completed_pieces, torrent.total_pieces) === "" && torrent.connected_peers === 0}&nbsp;{/if}
    </div>
    <div class="sub faint">
      ↓ всего {formatBytes(torrent.downloaded_bytes)}
      · ↑ всего {formatBytes(torrent.uploaded_bytes)}
      {#if torrent.downloaded_bytes === 0 && torrent.uploaded_bytes === 0}&nbsp;{/if}
    </div>
  </div>
  <div class="cell">
    <span class="badge" style={`--c: ${badge.color}`} title={errorText}>
      {badge.label}
    </span>
  </div>
  <div class="cell progress-cell">
    <PieceBar pieces={torrent.pieces} totalPieces={torrent.total_pieces} {percent} {doneColor} />
    <span class="pct">{percent}%</span>
  </div>
  <!-- Скорость 0 — серым, но элемент на месте: никакой перестановки строки. -->
  <div class="cell num" class:off={torrent.download_speed_bps === 0}>
    ↓ {formatSpeed(torrent.download_speed_bps)}
  </div>
  <div class="cell num" class:off={torrent.upload_speed_bps === 0}>
    ↑ {formatSpeed(torrent.upload_speed_bps)}
  </div>
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

  .name-cell {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .name {
    font-weight: 700;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .sub {
    font-size: 11px;
    line-height: 15px;
    color: var(--text-dim);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .faint {
    color: var(--text-faint);
  }

  .badge {
    display: inline-block;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: 11px;
    color: var(--c);
    background: color-mix(in srgb, var(--c) 12%, transparent);
    border-radius: var(--radius);
    padding: 2px 8px;
    white-space: nowrap;
  }

  .progress-cell {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .pct {
    min-width: 34px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--text-dim);
  }

  .num {
    text-align: right;
    font-size: 12px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .num:not(.off) {
    color: var(--text);
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
