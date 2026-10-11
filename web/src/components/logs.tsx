import { useInfiniteQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import { errorMessage, formatTime } from "../format";
import type { Schemas } from "../types";
import { Button, Empty, Select, TextInput } from "./ui";

type LogView = Schemas["LogView"];
type LogPage = Schemas["LogPage"];

export const LOG_SOURCES = ["app", "panel", "proxy", "cache", "agent", "system"] as const;
const LEVELS = ["error", "warn", "info", "debug", "trace"] as const;
const RANGES = [
  ["15m", "Last 15 minutes", 15 * 60],
  ["1h", "Last hour", 60 * 60],
  ["24h", "Last 24 hours", 24 * 60 * 60],
  ["7d", "Last 7 days", 7 * 24 * 60 * 60],
  ["30d", "Last 30 days", 30 * 24 * 60 * 60],
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
  error: "text-red-600 dark:text-red-400",
  warn: "text-amber-600 dark:text-amber-400",
  info: "text-sky-700 dark:text-sky-300",
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
  const [filters, setFilters] = useState<LogFilters>({
    source: "",
    level: "",
    status: "",
    q: "",
    range: "24h",
  });
  const [live, setLive] = useState(false);
  const [liveRows, setLiveRows] = useState<LogView[]>([]);
  const [liveNote, setLiveNote] = useState<string | null>(null);
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
    setLiveNote("Connecting…");
    const ws = new WebSocket(tailUrl(logParams(debounced, fixed, false)));
    socket.current = ws;
    ws.onopen = () => setLiveNote("Streaming new lines");
    ws.onclose = () => setLiveNote("Live tail disconnected");
    ws.onerror = () => setLiveNote("Live tail failed to connect");
    ws.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as
        { type: "entry"; entry: LogView } | { type: "lagged"; skipped: number };
      if (message.type === "entry") {
        setLiveRows((rows) => [message.entry, ...rows].slice(0, LIVE_LIMIT));
      } else {
        setLiveNote(`Fell behind; skipped ${message.skipped} lines`);
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
    <div className="space-y-3">
      <div className="grid gap-2 md:grid-cols-6">
        <TextInput
          className="md:col-span-2"
          placeholder="Search messages"
          value={filters.q}
          onChange={(event) => set("q", event.target.value)}
          aria-label="Search messages"
        />
        <Select
          value={filters.source}
          onChange={(event) => set("source", event.target.value)}
          aria-label="Source"
        >
          <option value="">All sources</option>
          {sources.map((source) => (
            <option key={source} value={source}>
              {source}
            </option>
          ))}
        </Select>
        <Select
          value={filters.level}
          onChange={(event) => set("level", event.target.value)}
          aria-label="Level"
        >
          <option value="">All levels</option>
          <option value="error">error</option>
          <option value="warn,error">warn and above</option>
          {LEVELS.slice(2).map((level) => (
            <option key={level} value={level}>
              {level}
            </option>
          ))}
        </Select>
        <Select
          value={filters.status}
          onChange={(event) => set("status", event.target.value)}
          aria-label="HTTP status"
        >
          <option value="">Any status</option>
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
          aria-label="Time range"
        >
          {RANGES.map(([key, label]) => (
            <option key={key} value={key}>
              {label}
            </option>
          ))}
        </Select>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <Button quiet={!live} onClick={() => setLive((value) => !value)}>
          {live ? "Stop live tail" : "Live tail"}
        </Button>
        <a
          className="inline-flex h-10 items-center rounded-md border border-line bg-panel px-3 text-sm hover:bg-paper"
          href={`/api/v1/logs/export?${exportBase.toString()}&format=ndjson`}
        >
          Export NDJSON
        </a>
        <a
          className="inline-flex h-10 items-center rounded-md border border-line bg-panel px-3 text-sm hover:bg-paper"
          href={`/api/v1/logs/export?${exportBase.toString()}&format=csv`}
        >
          Export CSV
        </a>
        <span className="text-sm text-muted">
          {live ? liveNote : search.isFetching ? "Loading…" : `${rows.length} lines`}
        </span>
      </div>
      {search.error && !live ? (
        <p className="text-sm text-red-600">{errorMessage(search.error, "Search failed")}</p>
      ) : null}
      <div className="overflow-x-auto rounded-lg border border-line bg-paper">
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
            <Empty>{live ? "Waiting for new lines…" : "No log lines match these filters."}</Empty>
          </div>
        ) : null}
      </div>
      {!live && search.hasNextPage ? (
        <Button quiet disabled={search.isFetchingNextPage} onClick={() => search.fetchNextPage()}>
          {search.isFetchingNextPage ? "Loading…" : "Load older lines"}
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
        className="cursor-pointer border-b border-line/60 align-top hover:bg-panel"
        onClick={onToggle}
      >
        <td className="px-2 py-1 whitespace-nowrap text-muted">{formatTime(row.logged_at)}</td>
        <td className="px-2 py-1 whitespace-nowrap text-muted">{row.source}</td>
        <td className={`px-2 py-1 whitespace-nowrap ${levelTone[row.level] ?? ""}`}>{row.level}</td>
        <td className="px-2 py-1 whitespace-nowrap">{row.http_status ?? ""}</td>
        <td className="w-full px-2 py-1 break-all whitespace-pre-wrap">{row.message}</td>
      </tr>
      {open && hasFields ? (
        <tr className="border-b border-line/60 bg-panel">
          <td colSpan={5} className="px-2 py-2">
            <pre className="whitespace-pre-wrap text-muted">{JSON.stringify(fields, null, 2)}</pre>
          </td>
        </tr>
      ) : null}
    </>
  );
}
