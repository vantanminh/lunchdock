import { currentLanguage, currentLocale, translate } from "./i18n";

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null) {
    return "—";
  }
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

export function formatDuration(ms: number | null | undefined): string {
  if (ms == null) {
    return "—";
  }
  if (ms < 1000) {
    return `${ms} ms`;
  }
  const seconds = ms / 1000;
  if (seconds < 120) {
    return `${seconds.toFixed(1)} s`;
  }
  return `${Math.floor(seconds / 60)} min ${Math.round(seconds % 60)} s`;
}

export function formatTime(value: string | null | undefined): string {
  return value ? new Date(value).toLocaleString(currentLocale()) : "—";
}

export function formatDate(value: string | null | undefined): string {
  return value ? new Date(value).toLocaleDateString(currentLocale(), { dateStyle: "medium" }) : "—";
}

export function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

const relativeFormats = new Map<string, Intl.RelativeTimeFormat>();

function relativeFormat(): Intl.RelativeTimeFormat {
  const locale = currentLocale();
  let format = relativeFormats.get(locale);
  if (!format) {
    format = new Intl.RelativeTimeFormat(locale, { numeric: "auto", style: "short" });
    relativeFormats.set(locale, format);
  }
  return format;
}

export function formatRelative(value: string | null | undefined, now = Date.now()): string {
  if (!value) {
    return "—";
  }
  const seconds = Math.round((new Date(value).getTime() - now) / 1000);
  const steps: [Intl.RelativeTimeFormatUnit, number][] = [
    ["second", 60],
    ["minute", 60],
    ["hour", 24],
    ["day", 30],
    ["month", 12],
  ];
  let amount = seconds;
  for (const [unit, size] of steps) {
    if (Math.abs(amount) < size) {
      return unit === "second" && Math.abs(amount) < 30
        ? String(translate(currentLanguage(), "just now"))
        : relativeFormat().format(amount, unit);
    }
    amount = Math.round(amount / size);
  }
  return relativeFormat().format(amount, "year");
}
