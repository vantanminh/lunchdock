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
import { Empty, Select } from "./ui";

export const RANGES = ["15m", "1h", "6h", "24h", "7d", "30d"] as const;
export type Range = (typeof RANGES)[number];

export type Unit = "percent" | "bytes" | "bps" | "count" | "plain";

export interface ChartSpec {
  title: string;
  metrics: string[];
  labels?: Record<string, string>;
  unit: Unit;
}

const COLORS = ["#2563eb", "#16a34a", "#d97706", "#dc2626", "#7c3aed", "#0891b2"];

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

export function RangePicker({ value, onChange }: { value: Range; onChange: (r: Range) => void }) {
  return (
    <Select
      aria-label="Time range"
      className="w-28"
      value={value}
      onChange={(event) => onChange(event.target.value as Range)}
    >
      {RANGES.map((range) => (
        <option key={range} value={range}>
          {range}
        </option>
      ))}
    </Select>
  );
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
      const t = new Date(point.t).getTime();
      const row = rows.get(t) ?? ({ t } as { t: number } & Record<string, number>);
      row[series.metric] = point.avg;
      rows.set(t, row);
    }
  }
  const points = [...rows.values()].sort((a, b) => a.t - b.t);

  return (
    <div className="rounded-lg border border-line p-3">
      <div className="mb-2 flex items-baseline justify-between gap-2">
        <h3 className="text-sm font-semibold">{spec.title}</h3>
        {data.data ? (
          <span className="text-xs text-muted">resolution {data.data.resolution}</span>
        ) : null}
      </div>
      {points.length === 0 ? (
        <div className="flex h-44 items-center justify-center">
          <Empty>{data.isLoading ? "Loading…" : "No samples in this range yet."}</Empty>
        </div>
      ) : (
        <ResponsiveContainer width="100%" height={176}>
          <AreaChart data={points} margin={{ top: 4, right: 8, bottom: 0, left: 0 }}>
            <CartesianGrid strokeDasharray="3 3" stroke="var(--line)" />
            <XAxis
              dataKey="t"
              type="number"
              scale="time"
              domain={["dataMin", "dataMax"]}
              tickFormatter={(value: number) => tickTime(value, range)}
              tick={TICK}
              axisLine={AXIS_LINE}
              tickLine={AXIS_LINE}
              minTickGap={32}
            />
            <YAxis
              width={64}
              tick={TICK}
              axisLine={AXIS_LINE}
              tickLine={AXIS_LINE}
              domain={spec.unit === "percent" ? [0, 100] : [0, "auto"]}
              tickFormatter={(value: number) => formatValue(value, spec.unit)}
            />
            <Tooltip
              {...TOOLTIP_STYLES}
              labelFormatter={(value) => new Date(Number(value)).toLocaleString()}
              formatter={(value, name) => [
                formatValue(Number(value), spec.unit),
                spec.labels?.[String(name)] ?? String(name),
              ]}
            />
            {spec.metrics.length > 1 ? (
              <Legend
                formatter={(value: string) => spec.labels?.[value] ?? value}
                wrapperStyle={{ fontSize: 12, color: "var(--ink-soft)" }}
              />
            ) : null}
            {spec.metrics.map((metric, index) => (
              <Area
                key={metric}
                type="monotone"
                dataKey={metric}
                stroke={COLORS[index % COLORS.length]}
                fill={COLORS[index % COLORS.length]}
                fillOpacity={0.12}
                strokeWidth={1.5}
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
  const [range, setRange] = useState<Range>("1h");
  return (
    <div className="space-y-3">
      <div className="flex items-center justify-end gap-2 text-sm text-muted">
        Range <RangePicker value={range} onChange={setRange} />
      </div>
      <div className="grid gap-3 md:grid-cols-2">
        {charts.map((spec) => (
          <MetricChart key={spec.title} spec={spec} range={range} target={target} />
        ))}
      </div>
    </div>
  );
}

export const SERVER_CHARTS: ChartSpec[] = [
  { title: "CPU", metrics: ["cpu_percent"], unit: "percent" },
  { title: "Memory", metrics: ["memory_percent"], unit: "percent" },
  { title: "Disk", metrics: ["disk_percent"], unit: "percent" },
  {
    title: "Network",
    metrics: ["net_rx_bps", "net_tx_bps"],
    labels: { net_rx_bps: "received", net_tx_bps: "sent" },
    unit: "bps",
  },
  { title: "Load (1 min)", metrics: ["load1"], unit: "plain" },
];

export const SERVICE_CHARTS: ChartSpec[] = [
  { title: "CPU", metrics: ["cpu_percent"], unit: "percent" },
  { title: "Memory", metrics: ["memory_bytes"], unit: "bytes" },
  {
    title: "Network",
    metrics: ["net_rx_bps", "net_tx_bps"],
    labels: { net_rx_bps: "received", net_tx_bps: "sent" },
    unit: "bps",
  },
  {
    title: "HTTP requests per minute",
    metrics: ["requests", "http_5xx"],
    labels: { requests: "requests", http_5xx: "5xx" },
    unit: "count",
  },
];
