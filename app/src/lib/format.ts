// Форматирование байт/скоростей для UI.

const KIB = 1024;
const UNITS = ["Б", "КиБ", "МиБ", "ГиБ", "ТиБ"];

/// Байты → человекочитаемый размер: `0` → `"0 Б"`, `1536` → `"1.5 КиБ"`.
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 Б";
  let value = bytes;
  let unit = 0;
  while (value >= KIB && unit < UNITS.length - 1) {
    value /= KIB;
    unit += 1;
  }
  const rounded = unit === 0 ? String(Math.round(value)) : value.toFixed(1);
  return `${rounded} ${UNITS[unit]}`;
}

/// Скорость байт/с → `"... /с"`.
export function formatSpeed(bps: number): string {
  return `${formatBytes(bps)}/с`;
}

/// Прогресс по кускам → проценты 0–100 (0 кусков — 0%).
export function progressPercent(completed: number, total: number): number {
  if (total <= 0) return 0;
  if (completed >= total) return 100;
  return Math.floor((completed / total) * 100);
}

/// Скачано по кускам → `"123 из 456"` или пусто, если метаданных нет.
export function piecesLabel(completed: number, total: number): string {
  if (total <= 0) return "";
  return `${completed} из ${total}`;
}

/// Оценка времени до конца загрузки (секунды) → `"~2 ч 15 мин"`, `"~5 мин"`,
/// `"~40 с"`. Компактно: два старших порядка.
export function formatEta(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "";
  const s = Math.ceil(seconds);
  const days = Math.floor(s / 86400);
  const hours = Math.floor((s % 86400) / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const secs = s % 60;
  if (days > 0) return `~${days} д ${hours} ч`;
  if (hours > 0) return `~${hours} ч ${minutes} мин`;
  if (minutes > 0) return `~${minutes} мин`;
  return `~${secs} с`;
}
