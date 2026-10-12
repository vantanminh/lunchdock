import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import {
  Area,
  AreaChart,
  CartesianGrid,
  Legend,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { api } from "../api";
import { formatBytes } from "../format";
import type { Schemas } from "../types";
import { Empty, Segmented } from "./ui";
import { msg, useT } from "../i18n";

export const RANGES = ["15m", "1h", "6h", "24h", "7d", "30d"] as const;
export type Range = (typeof RANGES)[number];

export type Unit = "percent" | "bytes" | "bps" | "count" | "plain";

export interface ChartSpec {
  title: string;
  metrics: string[];
  labels?: Record<string, string>;
  unit: Unit;
  /** One colour per metric; defaults to the shared palette. */
  colors?: string[];
}

// Series colours follow the theme: pine first, then amber and slate; red is kept for errors.
const COLORS = ["var(--pine)", "var(--amber)", "#5b7fa6", "var(--danger)"];

// Recharts paints its own white tooltip and grey ticks; route them through the theme tokens
// so text stays readable in both light and dark mode.
const TICK = { fontSize: 11, fill: "var(--muted)" };
const AXIS_LINE = { stroke: "var(--line-strong)" };
const TOOLTIP_STYLES = {
  contentStyle: {
    backgroundColor: "var(--surface)",
    border: "1px solid var(--line-strong)",
    borderRadius: 8,
    boxShadow: "var(--shadow-raised)",
    color: "var(--ink)",
    fontSize: 12,
  },
  labelStyle: { color: "var(--ink-soft)", marginBottom: 4 },
  itemStyle: { color: "var(--ink)" },
  cursor: { stroke: "var(--faint)" },
} as const;

export function formatValue(value: number | null | undefined, unit: Unit): string {
  if (value == null || Number.isNaN(value)) {
    return "—";
  }
  switch (unit) {
    case "percent":
      return `${value.toFixed(1)}%`;
    case "bytes":
      return formatBytes(value);
    case "bps":
      return `${formatBytes(value)}/s`;
    case "count":
      return Math.round(value).toLocaleString();
    default:
      return value.toFixed(2);
  }
}

function tickTime(value: number, range: Range): string {
  const date = new Date(value);
  return range === "7d" || range === "30d"
    ? date.toLocaleDateString(undefined, { month: "short", day: "numeric" })
    : date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

function MetricChart({
  spec,
  range,
  target,
}: {
  spec: ChartSpec;
  range: Range;
  target: { server_id?: string; service_id?: string };
}) {
  const t = useT();
  const params = new URLSearchParams({ metrics: spec.metrics.join(","), range });
  if (target.server_id) params.set("server_id", target.server_id);
  if (target.service_id) params.set("service_id", target.service_id);
  const data = useQuery({
    queryKey: ["metrics", target, spec.metrics, range],
    queryFn: () => api<Schemas["MetricsView"]>(`/api/v1/metrics?${params.toString()}`),
    refetchInterval: 10_000,
  });

  const rows = new Map<number, { t: number } & Record<string, number>>();
  for (const series of data.data?.series ?? []) {
    for (const point of series.points) {
      const time = new Date(point.t).getTime();
      const row = rows.get(time) ?? ({ t: time } as { t: number } & Record<string, number>);
      row[series.metric] = point.avg;
      rows.set(time, row);
    }
  }
  const points = [...rows.values()].sort((a, b) => a.t - b.t);
  const colors = spec.colors ?? COLORS;
  const latest = points.length ? points[points.length - 1]?.[spec.metrics[0] ?? ""] : undefined;

  return (
    <div className="min-w-0 rounded-card border border-line bg-surface p-4 sm:p-5">
      <div className="mb-3 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="text-sm font-semibold text-ink">{t(spec.title)}</h3>
          {data.data ? (
            <p className="mt-0.5 text-xs text-muted">
              {t("resolution {value}", { value: data.data.resolution })}
            </p>
          ) : null}
        </div>
        {latest != null ? (
          <p className="text-right">
            <span className="block text-lg leading-tight font-semibold text-ink tabular-nums">
              {formatValue(latest, spec.unit)}
            </span>
            <span className="text-xs text-muted">{t("latest")}</span>
          </p>
        ) : null}
      </div>
      {points.length === 0 ? (
        <div className="flex h-44 items-center justify-center">
          <Empty>{data.isLoading ? t("Loading…") : t("No samples in this range yet.")}</Empty>
        </div>
      ) : (
        <ResponsiveContainer width="100%" height={176}>
          <AreaChart data={points} margin={{ top: 4, right: 8, bottom: 0, left: 0 }}>
            <CartesianGrid strokeDasharray="3 3" stroke="var(--line)" vertical={false} />
            <XAxis
              dataKey="t"
              type="number"
              scale="time"
              domain={["dataMin", "dataMax"]}
              tickFormatter={(value: number) => tickTime(value, range)}
              tick={TICK}
              axisLine={AXIS_LINE}
              tickLine={false}
              minTickGap={32}
            />
            <YAxis
              width={78}
              tick={TICK}
              axisLine={false}
              tickLine={false}
              domain={spec.unit === "percent" ? [0, 100] : [0, "auto"]}
              tickFormatter={(value: number) => formatValue(value, spec.unit)}
            />
            <Tooltip
              {...TOOLTIP_STYLES}
              labelFormatter={(value) => new Date(Number(value)).toLocaleString()}
              formatter={(value, name) => [
                formatValue(Number(value), spec.unit),
                t(spec.labels?.[String(name)] ?? String(name)),
              ]}
            />
            {spec.metrics.length > 1 ? (
              <Legend
                formatter={(value: string) => t(spec.labels?.[value] ?? value)}
                wrapperStyle={{ fontSize: 12, color: "var(--ink-soft)" }}
              />
            ) : null}
            {spec.metrics.map((metric, index) => (
              <Area
                key={metric}
                type="monotone"
                dataKey={metric}
                stroke={colors[index % colors.length]}
                fill={colors[index % colors.length]}
                fillOpacity={0.1}
                strokeWidth={1.75}
                isAnimationActive={false}
                connectNulls
              />
            ))}
          </AreaChart>
        </ResponsiveContainer>
      )}
    </div>
  );
}

export function MetricsGrid({
  charts,
  target,
}: {
  charts: ChartSpec[];
  target: { server_id?: string; service_id?: string };
}) {
  const t = useT();
  const [range, setRange] = useState<Range>("1h");
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm text-muted">{t("Averages over the selected time range.")}</p>
        <div className="max-w-full overflow-x-auto">
          <Segmented<Range>
            label={t("Time range")}
            value={range}
            onChange={setRange}
            options={RANGES.map((value) => [value, value] as const)}
          />
        </div>
      </div>
      <div className="grid gap-4 md:grid-cols-2">
        {charts.map((spec) => (
          <MetricChart key={spec.title} spec={spec} range={range} target={target} />
        ))}
      </div>
    </div>
  );
}

export const SERVER_CHARTS: ChartSpec[] = [
  { title: msg("CPU"), metrics: ["cpu_percent"], unit: "percent" },
  { title: msg("Memory"), metrics: ["memory_percent"], unit: "percent" },
  { title: msg("Disk"), metrics: ["disk_percent"], unit: "percent" },
  {
    title: msg("Network"),
    metrics: ["net_rx_bps", "net_tx_bps"],
    labels: { net_rx_bps: msg("received"), net_tx_bps: msg("sent") },
    unit: "bps",
  },
  { title: msg("Load (1 min)"), metrics: ["load1"], unit: "plain" },
];

export const SERVICE_CHARTS: ChartSpec[] = [
  { title: msg("CPU"), metrics: ["cpu_percent"], unit: "percent" },
  { title: msg("Memory"), metrics: ["memory_bytes"], unit: "bytes" },
  {
    title: msg("Network"),
    metrics: ["net_rx_bps", "net_tx_bps"],
    labels: { net_rx_bps: msg("received"), net_tx_bps: msg("sent") },
    unit: "bps",
  },
  {
    title: msg("HTTP requests per minute"),
    metrics: ["requests", "http_5xx"],
    labels: { requests: msg("requests"), http_5xx: "5xx" },
    unit: "count",
    colors: ["var(--pine)", "var(--danger)"],
  },
];
