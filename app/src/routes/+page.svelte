<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { open } from "@tauri-apps/plugin-dialog";
  import TorrentRow from "../lib/TorrentRow.svelte";
  import type { TorrentEvent, TorrentStatus } from "../lib/types";
  import { formatSpeed } from "../lib/format";

  let torrents = $state<TorrentStatus[]>([]);
  let defaultDir = $state("");
  let banner = $state(""); // неблокирующее сообщение об ошибке
  let bannerTimer: ReturnType<typeof setTimeout> | undefined;
  let dragOver = $state(false);
  let magnetOpen = $state(false);
  let magnetUri = $state("");
  let magnetBusy = $state(false);
  let pendingRemove = $state<TorrentStatus | null>(null);
  let deleteFiles = $state(false);

  function showError(msg: string) {
    banner = msg;
    clearTimeout(bannerTimer);
    bannerTimer = setTimeout(() => (banner = ""), 6000);
  }

  function upsert(status: TorrentStatus) {
    const i = torrents.findIndex((t) => t.handle === status.handle);
    if (i >= 0) torrents[i] = status;
    else torrents.push(status);
  }

  // Удержание скорости: при рваном сворме честные нули между всплесками
  // выглядят как мерцание. Нулевую скорость показываем только если она
  // держится дольше HOLD_MS; до этого — последнее ненулевое значение.
  const HOLD_MS = 4000;
  const speedHold = new Map<string, { down: SpeedVal; up: SpeedVal }>();
  type SpeedVal = { v: number; since: number };
  function holdSpeed(fresh: number, last: SpeedVal | undefined, now: number): SpeedVal {
    if (fresh > 0) return { v: fresh, since: now };
    if (last && now - last.since < HOLD_MS) return last;
    return { v: 0, since: now };
  }
  function smoothSpeed(status: TorrentStatus): TorrentStatus {
    const now = Date.now();
    const prev = speedHold.get(status.handle);
    const down = holdSpeed(status.download_speed_bps, prev?.down, now);
    const up = holdSpeed(status.upload_speed_bps, prev?.up, now);
    speedHold.set(status.handle, { down, up });
    return { ...status, download_speed_bps: down.v, upload_speed_bps: up.v };
  }

  function applyEvent(event: TorrentEvent) {
    if ("Added" in event || "Updated" in event) {
      upsert(smoothSpeed("Added" in event ? event.Added : event.Updated));
    } else if ("Removed" in event) {
      torrents = torrents.filter((t) => t.handle !== event.Removed);
      speedHold.delete(event.Removed);
    } else if ("Error" in event) {
      showError(event.Error.error);
    }
  }

  async function addTorrentFiles(paths: string[]) {
    for (const path of paths) {
      try {
        await invoke("add_torrent_file", { path, downloadDir: defaultDir });
      } catch (e) {
        showError(String(e));
      }
    }
  }

  async function pickTorrents() {
    const paths = await open({
      multiple: true,
      filters: [{ name: "Torrent", extensions: ["torrent"] }],
    });
    if (paths) await addTorrentFiles(paths);
  }

  async function addMagnet() {
    const uri = magnetUri.trim();
    if (!uri) return;
    magnetBusy = true;
    try {
      await invoke("add_magnet", { uri, downloadDir: defaultDir });
      magnetUri = "";
      magnetOpen = false;
    } catch (e) {
      showError(String(e));
    } finally {
      magnetBusy = false;
    }
  }

  function askRemove(torrent: TorrentStatus) {
    pendingRemove = torrent;
    deleteFiles = false;
  }

  async function confirmRemove() {
    const target = pendingRemove;
    if (!target) return;
    pendingRemove = null;
    try {
      await invoke("remove_torrent", { handle: target.handle, deleteFiles });
    } catch (e) {
      showError(String(e));
    }
  }

  function pause(handle: string) {
    invoke("pause_torrent", { handle }).catch((e) => showError(String(e)));
  }

  function resume(handle: string) {
    invoke("resume_torrent", { handle }).catch((e) => showError(String(e)));
  }

  let totalDown = $derived(torrents.reduce((s, t) => s + t.download_speed_bps, 0));
  let totalUp = $derived(torrents.reduce((s, t) => s + t.upload_speed_bps, 0));

  $effect(() => {
    let disposed = false;
    let unlisteners: (() => void)[] = [];

    (async () => {
      try {
        const [statuses, dir] = await Promise.all([
          invoke<TorrentStatus[]>("get_all_statuses"),
          invoke<string>("get_default_download_dir"),
        ]);
        if (disposed) return;
        torrents = statuses;
        defaultDir = dir;

        const unEvent = await listen<TorrentEvent>("torrent-event", (e) =>
          applyEvent(e.payload),
        );
        const unDrag = await getCurrentWebview().onDragDropEvent((event) => {
          if (event.payload.type === "enter" || event.payload.type === "over") {
            dragOver = true;
          } else if (event.payload.type === "leave") {
            dragOver = false;
          } else if (event.payload.type === "drop") {
            dragOver = false;
            const paths = event.payload.paths.filter((p) =>
              p.endsWith(".torrent"),
            );
            if (paths.length > 0) void addTorrentFiles(paths);
          }
        });
        if (disposed) {
          unEvent();
          unDrag();
          return;
        }
        unlisteners = [unEvent, unDrag];
      } catch (e) {
        showError(String(e));
      }
    })();

    return () => {
      disposed = true;
      for (const un of unlisteners) un();
      clearTimeout(bannerTimer);
    };
  });
</script>

<div class="app">
  <header>
    <span class="title">RustTorrent</span>
    <span class="spacer"></span>
    <div class="totals">
      <span class:off={totalDown === 0}>↓ {formatSpeed(totalDown)}</span>
      <span class:off={totalUp === 0}>↑ {formatSpeed(totalUp)}</span>
    </div>
    <button onclick={() => (magnetOpen = !magnetOpen)}>magnet</button>
    <button class="primary" onclick={pickTorrents}>+ .torrent</button>
  </header>

  {#if magnetOpen}
    <div class="magnet-bar">
      <input
        type="text"
        placeholder="magnet:?xt=urn:btih:…"
        bind:value={magnetUri}
        onkeydown={(e) => e.key === "Enter" && addMagnet()}
      />
      <button class="primary" disabled={magnetBusy || !magnetUri.trim()} onclick={addMagnet}>
        Добавить
      </button>
      <button onclick={() => (magnetOpen = false)}>Отмена</button>
    </div>
  {/if}

  {#if banner}
    <div class="banner">{banner}</div>
  {/if}

  <main>
    {#if torrents.length === 0}
      <div class="empty">
        <p>Перетащите .torrent-файл сюда</p>
        <p class="hint">или нажмите «+ .torrent» / вставьте magnet-ссылку</p>
      </div>
    {:else}
      <section class="list">
        <div class="thead">
          <span>Торрент</span>
          <span>Статус</span>
          <span>Прогресс</span>
          <span class="num">Скачивание</span>
          <span class="num">Раздача</span>
          <span></span>
        </div>
        {#each torrents as torrent (torrent.handle)}
          <TorrentRow {torrent} onPause={pause} onResume={resume} onRemove={askRemove} />
        {/each}
      </section>
    {/if}
  </main>

  {#if dragOver}
    <div class="drop-overlay">Отпустите, чтобы добавить</div>
  {/if}

  {#if pendingRemove}
    <div class="overlay" role="presentation" onclick={(e) => e.target === e.currentTarget && (pendingRemove = null)}>
      <div class="modal">
        <h3>Удалить «{pendingRemove.name ?? pendingRemove.handle}»?</h3>
        <label>
          <input type="checkbox" bind:checked={deleteFiles} />
          Удалить скачанные файлы
        </label>
        <div class="modal-actions">
          <button onclick={() => (pendingRemove = null)}>Отмена</button>
          <button class="primary danger" onclick={confirmRemove}>Удалить</button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    position: relative;
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 16px;
    background: var(--bg);
    flex-shrink: 0;
  }

  .title {
    font-weight: 700;
    font-size: 15px;
  }

  .spacer {
    flex: 1;
  }

  .totals {
    display: flex;
    gap: 12px;
    margin-right: 12px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .totals .off {
    color: var(--text-faint);
  }

  .magnet-bar {
    display: flex;
    gap: 8px;
    padding: 0 16px 10px;
    background: var(--bg);
    flex-shrink: 0;
  }

  .magnet-bar input {
    flex: 1;
  }

  .banner {
    margin: 0 16px 8px;
    padding: 8px 12px;
    background: #2a1414;
    color: var(--red);
    border: 1px solid #3a1d1d;
    border-radius: var(--radius);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex-shrink: 0;
  }

  main {
    flex: 1;
    overflow-y: auto;
    /* Таблица во всю ширину окна — без боковых отступов. */
    padding: 0 0 16px;
  }

  .list {
    background: var(--bg-panel);
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }

  .thead {
    display: grid;
    grid-template-columns: var(--row-grid);
    padding: 10px 16px;
    border-bottom: 1px solid var(--border);
    font-size: 11px;
    color: var(--text-dim);
  }

  .thead .num {
    text-align: right;
  }

  .empty {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
    color: var(--text);
  }

  .empty p {
    margin: 0;
    font-size: 15px;
  }

  .empty .hint {
    font-size: 12px;
    color: var(--text-dim);
  }

  .drop-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 18px;
    font-weight: 600;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border: 2px dashed var(--accent);
    border-radius: var(--radius);
    margin: 8px;
    pointer-events: none;
    z-index: 10;
  }

  .overlay {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 70%);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 20;
  }

  .modal {
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 20px;
    min-width: 320px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .modal h3 {
    margin: 0;
    font-size: 14px;
    word-break: break-all;
  }

  .modal label {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-dim);
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .danger {
    color: var(--red);
    border-color: var(--red);
  }

  .danger:hover:not(:disabled) {
    background: color-mix(in srgb, var(--red) 15%, transparent);
    color: var(--red);
  }
</style>
