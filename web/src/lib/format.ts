/**
 * Formatting helpers. Sizes use decimal units like macOS Finder (1 GB =
 * 1,000,000,000 bytes), so numbers match what users see in their file manager
 * and on the drive's label.
 */

const UNITS = ["KB", "MB", "GB", "TB", "PB"] as const;

const integer = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });

/** `1234` → `1,234` in the user's locale. */
export function formatCount(n: number): string {
  return integer.format(n);
}

/** Fraction digits for a value in a unit: whole KB, then 2, 1 or 0 by size. */
function byteDigits(unit: number, value: number): number {
  if (unit === 0) return 0;
  const abs = Math.abs(value);
  return abs < 10 ? 2 : abs < 100 ? 1 : 0;
}

/** Decimal bytes like Finder: `1.2 GB`, `345.6 MB`, `12 KB`, `980 bytes`. */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return "—";
  const whole = Math.round(bytes);
  if (Math.abs(whole) < 1000) return `${whole} ${Math.abs(whole) === 1 ? "byte" : "bytes"}`;
  let value = bytes / 1000;
  let unit = 0;
  // Round first, then promote, so 999.6 KB reads "1 MB" rather than "1,000 KB".
  for (;;) {
    const digits = byteDigits(unit, value);
    const factor = 10 ** digits;
    const rounded = Math.round(value * factor) / factor;
    if (Math.abs(rounded) >= 1000 && unit < UNITS.length - 1) {
      value /= 1000;
      unit += 1;
      continue;
    }
    const text = new Intl.NumberFormat(undefined, {
      maximumFractionDigits: digits,
      minimumFractionDigits: 0,
    }).format(rounded);
    return `${text} ${UNITS[unit]}`;
  }
}

/** Split a byte count into number and unit, for large display type. */
export function splitBytes(bytes: number): { value: string; unit: string } {
  const text = formatBytes(bytes);
  const at = text.lastIndexOf(" ");
  return { value: text.slice(0, at), unit: text.slice(at + 1) };
}

/** Percent with no decimals, clamped to 0..100 for display. */
export function formatPercent(value: number, digits = 0): string {
  const clamped = Math.max(0, Math.min(100, value));
  return `${clamped.toFixed(digits)}%`;
}

/** Share of `part` in `whole` as 0..100, or 0 when whole is 0. */
export function percentOf(part: number, whole: number): number {
  if (!whole) return 0;
  return (part / whole) * 100;
}

/** Media duration as a clock: `1:42:10` or `4:05`. */
export function formatClock(secs: number | null | undefined): string {
  if (secs === null || secs === undefined || !Number.isFinite(secs)) return "—";
  const total = Math.max(0, Math.round(secs));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return h > 0 ? `${h}:${mm}:${String(s).padStart(2, "0")}` : `${mm}:${String(s).padStart(2, "0")}`;
}

/** Elapsed time in words: `45 s`, `12 min`, `1 h 20 min`, `2 days`. */
export function formatDuration(secs: number | null | undefined): string {
  if (secs === null || secs === undefined || !Number.isFinite(secs)) return "—";
  const total = Math.max(0, Math.round(secs));
  if (total < 60) return `${total} s`;
  const minutes = Math.round(total / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (hours < 24) return rest ? `${hours} h ${rest} min` : `${hours} h`;
  const days = Math.floor(hours / 24);
  const restHours = hours % 24;
  const dayText = `${days} ${days === 1 ? "day" : "days"}`;
  return restHours ? `${dayText} ${restHours} h` : dayText;
}

/** Remaining time in plain words: `about 12 min left`. */
export function formatEta(secs: number | null | undefined): string | null {
  if (secs === null || secs === undefined || !Number.isFinite(secs)) return null;
  if (secs < 45) return "less than a minute left";
  if (secs < 90) return "about a minute left";
  return `about ${formatDuration(roundEta(secs))} left`;
}

/** Round ETAs so they don't flicker: minutes under an hour, 5 min above. */
function roundEta(secs: number): number {
  if (secs < 3600) return Math.round(secs / 60) * 60;
  return Math.round(secs / 300) * 300;
}

const relative = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
const shortDate = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
const longDate = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "short",
  day: "numeric",
});
const dateTime = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** `just now`, `3 min ago`, `2 h ago`, `yesterday`, `Sep 12`. */
export function formatRelative(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "—";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";
  const diff = Math.round((then - now) / 1000);
  const abs = Math.abs(diff);
  if (abs < 45) return "just now";
  if (abs < 3600) {
    const minutes = Math.round(diff / 60);
    return diff < 0 ? `${-minutes} min ago` : `in ${minutes} min`;
  }
  if (abs < 86_400) {
    const hours = Math.round(diff / 3600);
    return diff < 0 ? `${-hours} h ago` : `in ${hours} h`;
  }
  if (abs < 7 * 86_400) return relative.format(Math.round(diff / 86_400), "day");
  const date = new Date(then);
  return date.getFullYear() === new Date(now).getFullYear()
    ? shortDate.format(date)
    : longDate.format(date);
}

/** Full local date and time, for tooltips and detail views. */
export function formatDateTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const then = Date.parse(iso);
  return Number.isNaN(then) ? "—" : dateTime.format(new Date(then));
}

/** `YYYY-MM-DD` → `Sep 12`. */
export function formatDay(day: string): string {
  const [y, m, d] = day.split("-").map(Number);
  if (!y || !m || !d) return day;
  return shortDate.format(new Date(y, m - 1, d));
}

/** Hour of day on a 24 h clock: `01:00`. */
export function formatHour(hour: number): string {
  return `${String(((hour % 24) + 24) % 24).padStart(2, "0")}:00`;
}

/** Bitrate in bit/s → `8.4 Mb/s`. */
export function formatBitrate(bps: number | null | undefined): string {
  if (!bps) return "—";
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mb/s`;
  return `${Math.round(bps / 1000)} kb/s`;
}

/** `3` → `3 files`, `1` → `1 file`. */
export function plural(n: number, singular: string, pluralForm = `${singular}s`): string {
  return `${formatCount(n)} ${n === 1 ? singular : pluralForm}`;
}

/** Shorten a long file name in the middle, keeping the extension visible. */
export function middleTruncate(text: string, max = 48): string {
  if (text.length <= max) return text;
  const keepEnd = Math.min(16, Math.floor(max / 3));
  return `${text.slice(0, max - keepEnd - 1)}…${text.slice(-keepEnd)}`;
}
