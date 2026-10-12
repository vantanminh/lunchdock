import { useInfiniteQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import { errorMessage, formatTime } from "../format";
import type { Schemas } from "../types";
import { Icon } from "./icons";
import { Button, buttonClass, Empty, Select, TextInput } from "./ui";
import { msg, useT } from "../i18n";

type LogView = Schemas["LogView"];
type LogPage = Schemas["LogPage"];

export const LOG_SOURCES = ["app", "panel", "proxy", "cache", "agent", "system"] as const;
const LEVELS = ["error", "warn", "info", "debug", "trace"] as const;
const RANGES = [
  ["15m", msg("Last 15 minutes"), 15 * 60],
  ["1h", msg("Last hour"), 60 * 60],
  ["24h", msg("Last 24 hours"), 24 * 60 * 60],
  ["7d", msg("Last 7 days"), 7 * 24 * 60 * 60],
  ["30d", msg("Last 30 days"), 30 * 24 * 60 * 60],
] as const;
const LIVE_LIMIT = 1000;

export interface LogFilters {
  source: string;
  level: string;
  status: string;
  q: string;
  range: string;
}

const levelTone: Record<string, string> = {
  error: "font-medium text-danger",
  warn: "font-medium text-amber",
  info: "text-pine",
  debug: "text-muted",
  trace: "text-muted",
};

export function logParams(filters: LogFilters, fixed: Record<string, string>, withRange = true) {
  const params = new URLSearchParams(fixed);
  if (filters.source) params.set("source", filters.source);
  if (filters.level) params.set("level", filters.level);
  if (filters.status) params.set("status", filters.status);
  if (filters.q.trim()) params.set("q", filters.q.trim());
  if (withRange) {
    const seconds = RANGES.find(([key]) => key === filters.range)?.[2] ?? 24 * 60 * 60;
    params.set("since", new Date(Date.now() - seconds * 1000).toISOString());
  }
  return params;
}

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return debounced;
}

function tailUrl(params: URLSearchParams) {
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/api/v1/logs/tail?${params.toString()}`;
}

/** Search, live tail, and export over the central log store. */
export function LogViewer({
  fixed = {},
  sources = LOG_SOURCES,
}: {
  fixed?: Record<string, string>;
  sources?: readonly string[];
}) {
  const t = useT();
  const [filters, setFilters] = useState<LogFilters>({
    source: "",
    level: "",
    status: "",
    q: "",
    range: "24h",
  });
  const [live, setLive] = useState(false);
  const [liveRows, setLiveRows] = useState<LogView[]>([]);
  const [liveNote, setLiveNote] = useState<{
    key: string;
    values?: Record<string, number>;
  } | null>(null);
  const [expanded, setExpanded] = useState<number | null>(null);
  const debounced = useDebounced(filters, 300);
  const fixedKey = JSON.stringify(fixed);

  const search = useInfiniteQuery({
    queryKey: ["logs", fixedKey, debounced],
    initialPageParam: null as null | { time: string; id: number },
    queryFn: ({ pageParam }) => {
      const params = logParams(debounced, fixed);
      params.set("limit", "200");
      if (pageParam) {
        params.set("before_time", pageParam.time);
        params.set("before_id", String(pageParam.id));
      }
      return api<LogPage>(`/api/v1/logs?${params.toString()}`);
    },
    getNextPageParam: (last) =>
      last.next_before_time && last.next_before_id != null
        ? { time: last.next_before_time, id: last.next_before_id }
        : undefined,
    enabled: !live,
  });

  const socket = useRef<WebSocket | null>(null);
  useEffect(() => {
    if (!live) return;
    setLiveRows([]);
    setLiveNote({ key: "Connecting…" });
    const ws = new WebSocket(tailUrl(logParams(debounced, fixed, false)));
    socket.current = ws;
    ws.onopen = () => setLiveNote({ key: "Streaming new lines" });
    ws.onclose = () => setLiveNote({ key: "Live tail disconnected" });
    ws.onerror = () => setLiveNote({ key: "Live tail failed to connect" });
    ws.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as
        { type: "entry"; entry: LogView } | { type: "lagged"; skipped: number };
      if (message.type === "entry") {
        setLiveRows((rows) => [message.entry, ...rows].slice(0, LIVE_LIMIT));
      } else {
        setLiveNote({
          key: "Fell behind; skipped {count} lines",
          values: { count: message.skipped },
        });
      }
    };
    return () => ws.close();
  }, [live, debounced, fixedKey]);

  const rows = useMemo(
    () => (live ? liveRows : (search.data?.pages.flatMap((page) => page.entries) ?? [])),
    [live, liveRows, search.data],
  );
  const exportBase = logParams(filters, fixed);

  function set<K extends keyof LogFilters>(key: K, value: LogFilters[K]) {
    setFilters((current) => ({ ...current, [key]: value }));
  }

  return (
    <div className="@container space-y-3">
      <div className="grid grid-cols-2 gap-2 @2xl:grid-cols-[2fr_1fr_1fr_1fr_1.3fr]">
        <TextInput
          className="col-span-2 @2xl:col-span-1"
          placeholder={t("Search messages")}
          value={filters.q}
          onChange={(event) => set("q", event.target.value)}
          aria-label={t("Search messages")}
        />
        <Select
          value={filters.source}
          onChange={(event) => set("source", event.target.value)}
          aria-label={t("Source")}
        >
          <option value="">{t("All sources")}</option>
          {sources.map((source) => (
            <option key={source} value={source}>
              {source}
            </option>
          ))}
        </Select>
        <Select
          value={filters.level}
          onChange={(event) => set("level", event.target.value)}
          aria-label={t("Level")}
        >
          <option value="">{t("All levels")}</option>
          <option value="error">error</option>
          <option value="warn,error">{t("warn and above")}</option>
          {LEVELS.slice(2).map((level) => (
            <option key={level} value={level}>
              {level}
            </option>
          ))}
        </Select>
        <Select
          value={filters.status}
          onChange={(event) => set("status", event.target.value)}
          aria-label={t("HTTP status")}
        >
          <option value="">{t("Any status")}</option>
          {["2xx", "3xx", "4xx", "5xx"].map((status) => (
            <option key={status} value={status}>
              {status}
            </option>
          ))}
        </Select>
        <Select
          value={filters.range}
          disabled={live}
          onChange={(event) => set("range", event.target.value)}
          aria-label={t("Time range")}
        >
          {RANGES.map(([key, label]) => (
            <option key={key} value={key}>
              {t(label)}
            </option>
          ))}
        </Select>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant={live ? "primary" : "secondary"}
          size="sm"
          icon={live ? "stop" : "play"}
          onClick={() => setLive((value) => !value)}
        >
          {live ? t("Stop live tail") : t("Live tail")}
        </Button>
        <a
          className={buttonClass({ variant: "secondary", size: "sm" })}
          href={`/api/v1/logs/export?${exportBase.toString()}&format=ndjson`}
        >
          <Icon name="download" size={14} />
          {t("Export NDJSON")}
        </a>
        <a
          className={buttonClass({ variant: "secondary", size: "sm" })}
          href={`/api/v1/logs/export?${exportBase.toString()}&format=csv`}
        >
          <Icon name="download" size={14} />
          {t("Export CSV")}
        </a>
        <span className="ml-auto text-[13px] text-muted">
          {live
            ? liveNote && t(liveNote.key, liveNote.values)
            : search.isFetching
              ? t("Loading…")
              : t("{count} lines", { count: rows.length })}
        </span>
      </div>
      {search.error && !live ? (
        <p className="text-sm text-danger">{errorMessage(search.error, t("Search failed"))}</p>
      ) : null}
      <div className="max-h-[36rem] overflow-auto rounded-alert border border-line bg-surface">
        <table className="w-full font-mono text-xs">
          <tbody>
            {rows.map((row) => (
              <LogRow
                key={row.id}
                row={row}
                open={expanded === row.id}
                onToggle={() => setExpanded(expanded === row.id ? null : row.id)}
              />
            ))}
          </tbody>
        </table>
        {rows.length === 0 && !search.isFetching ? (
          <div className="p-4">
            <Empty>
              {live ? t("Waiting for new lines…") : t("No log lines match these filters.")}
            </Empty>
          </div>
        ) : null}
      </div>
      {!live && search.hasNextPage ? (
        <Button quiet disabled={search.isFetchingNextPage} onClick={() => search.fetchNextPage()}>
          {search.isFetchingNextPage ? t("Loading…") : t("Load older lines")}
        </Button>
      ) : null}
    </div>
  );
}

function LogRow({ row, open, onToggle }: { row: LogView; open: boolean; onToggle: () => void }) {
  const fields = row.fields as Record<string, unknown>;
  const hasFields = Object.keys(fields ?? {}).length > 0;
  return (
    <>
      <tr
        className={`border-b border-line/70 align-top transition-colors hover:bg-paper ${hasFields ? "cursor-pointer" : ""}`}
        onClick={hasFields ? onToggle : undefined}
      >
        <td className="px-3 py-1.5 whitespace-nowrap text-muted">{formatTime(row.logged_at)}</td>
        <td className="px-2 py-1.5 whitespace-nowrap text-muted">{row.source}</td>
        <td className={`px-2 py-1.5 whitespace-nowrap ${levelTone[row.level] ?? ""}`}>
          {row.level}
        </td>
        <td className="px-2 py-1.5 whitespace-nowrap text-ink-soft">{row.http_status ?? ""}</td>
        <td className="w-full px-2 py-1.5 pr-3 break-all whitespace-pre-wrap text-ink">
          {row.message}
        </td>
      </tr>
      {open && hasFields ? (
        <tr className="border-b border-line/70 bg-paper">
          <td colSpan={5} className="px-3 py-2">
            <pre className="whitespace-pre-wrap text-muted">{JSON.stringify(fields, null, 2)}</pre>
          </td>
        </tr>
      ) : null}
    </>
  );
}
