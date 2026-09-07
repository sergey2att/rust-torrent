// Типы событий/статусов daemon — зеркало serde-контракта крейта daemon
// (формат закреплён тестами status_serializes_for_ui / event_serializes_for_ui:
// unit-варианты — строки, варианты с данными — вложенный словарь без тега).

export type TorrentState =
  | "FetchingMetadata"
  | "Downloading"
  | "Seeding"
  | "Paused"
  | { Rechecking: { done: number; total: number } }
  | { Error: string };

export interface TorrentStatus {
  handle: string;
  name: string | null;
  state: TorrentState;
  completed_pieces: number;
  total_pieces: number;
  downloaded_bytes: number;
  uploaded_bytes: number;
  total_bytes: number;
  eta_seconds: number | null;
  download_speed_bps: number;
  upload_speed_bps: number;
  connected_peers: number;
  /** Упакованные состояния кусков (2 бита: 0 нет, 1 качается, 2 готов). */
  pieces: number[];
}

export type TorrentEvent =
  | { Added: TorrentStatus }
  | { Updated: TorrentStatus }
  | { Removed: string }
  | { Error: { handle: string; error: string } };
