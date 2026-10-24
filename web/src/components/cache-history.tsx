import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
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
import { toast } from "sonner";
import { api } from "../api";
import { formatBytes } from "../format";
import { useT } from "../i18n";
import { AXIS_LINE, COLORS, TICK, TOOLTIP_STYLES, formatValue } from "./metrics";
import { Button, Card, Empty, Field, Segmented, TextInput } from "./ui";

interface Settings {
  memory_percent: number;
  memory_cap_bytes: number | null;
  host_memory_bytes: number | null;
  effective_cap_bytes: number;
}

interface Point {
  sampled_at: string;
  hit_ratio: number | null;
  hits: number;
  misses: number;
  stored_bytes: number;
  memory_max_bytes: number;
  evictions: number;
  bytes_served: number;
}

interface ServerHistory {
  server_id: string;
  name: string;
  points: Point[];
}

export function CacheManagement() {
  const t = useT();
  const queryClient = useQueryClient();
  const settings = useQuery({
    queryKey: ["cache-settings"],
    queryFn: () => api<Settings>("/api/v1/cache/settings"),
  });
  const [percent, setPercent] = useState<string | null>(null);
  const [cap, setCap] = useState<string | null>(null);
  const [hours, setHours] = useState("24");
  const history = useQuery({
    queryKey: ["cache-history", hours],
    queryFn: () => api<ServerHistory[]>(`/api/v1/cache/history?hours=${hours}`),
    refetchInterval: 60_000,
  });

  const save = useMutation({
    mutationFn: () =>
      api<Settings>("/api/v1/cache/settings", {
        method: "PUT",
        body: JSON.stringify({
          memory_percent: Number(percent ?? settings.data?.memory_percent ?? 15),
          memory_cap_bytes: capMiB(cap, settings.data?.memory_cap_bytes),
        }),
      }),
    onSuccess: async () => {
      toast.success(t("Cache memory settings saved"));
      setPercent(null);
      setCap(null);
      await queryClient.invalidateQueries({ queryKey: ["cache-settings"] });
    },
    onError: (error) =>
      toast.error(error instanceof Error ? error.message : t("Could not save cache settings")),
  });

  const shownPercent = percent ?? String(settings.data?.memory_percent ?? "");
  const shownCap =
    cap ??
    (settings.data?.memory_cap_bytes == null
      ? ""
      : String(Math.round(settings.data.memory_cap_bytes / (1024 * 1024))));

  return (
    <>
      <Card title={t("Memory limit")}>
        <p className="mb-3 text-sm text-muted">
          {t(
            "The cache keeps responses in memory. Its budget is the sum of the policies in use, then limited to this share of the machine and to the ceiling. The default ceiling is 384 MiB. New installs also stop the cache container at 512 MiB (RUDDER_CACHE_MEMORY); raise that too if you set a higher ceiling.",
          )}
        </p>
        {settings.data ? (
          <p className="mb-3 text-sm">
            {t("In effect {cap}", { cap: formatBytes(settings.data.effective_cap_bytes) })}
            {settings.data.host_memory_bytes
              ? t(" · host {size}", { size: formatBytes(settings.data.host_memory_bytes) })
              : ""}
          </p>
        ) : null}
        <form
          className="grid gap-3 md:grid-cols-[12rem_12rem_auto]"
          onSubmit={(event) => {
            event.preventDefault();
            save.mutate();
          }}
        >
          <Field label={t("Share of the machine (%)")}>
            <TextInput
              inputMode="numeric"
              value={shownPercent}
              onChange={(event) => setPercent(event.target.value)}
            />
          </Field>
          <Field label={t("Ceiling (MiB, blank for 384)")}>
            <TextInput
              inputMode="numeric"
              value={shownCap}
              onChange={(event) => setCap(event.target.value)}
            />
          </Field>
          <div className="flex items-end">
            <Button type="submit" disabled={save.isPending}>
              {t("Save")}
            </Button>
          </div>
        </form>
      </Card>
      <Card title={t("Effectiveness")}>
        <div className="mb-3">
          <Segmented
            label={t("Time range")}
            value={hours}
            onChange={setHours}
            options={[
              ["1", t("1 hour")],
              ["6", t("6 hours")],
              ["24", t("24 hours")],
              ["168", t("7 days")],
            ]}
          />
        </div>
        {(history.data ?? []).length === 0 ? (
          <Empty>
            {t("Readings appear here after the cache has been checked for about a minute.")}
          </Empty>
        ) : (
          (history.data ?? []).map((server) => (
            <ServerChart key={server.server_id} server={server} />
          ))
        )}
      </Card>
    </>
  );
}

function capMiB(typed: string | null, saved: number | null | undefined): number | null {
  if (typed == null) return saved ?? null;
  const trimmed = typed.trim();
  if (!trimmed) return null;
  return Math.round(Number(trimmed) * 1024 * 1024);
}

function ServerChart({ server }: { server: ServerHistory }) {
  const t = useT();
  const data = server.points.map((point) => ({
    time: new Date(point.sampled_at).getTime(),
    ratio: point.hit_ratio == null ? null : point.hit_ratio * 100,
    stored: point.stored_bytes,
    budget: point.memory_max_bytes,
    evictions: point.evictions,
  }));
  const latest = server.points.at(-1);
  return (
    <section className="mb-6">
      <h3 className="mb-1 font-medium">{server.name}</h3>
      {latest ? (
        <p className="mb-2 text-sm text-muted">
          {t("Last sample: hit ratio {ratio} · {stored} stored · {evictions} evictions in the interval", {
            ratio: latest.hit_ratio == null ? t("n/a") : `${Math.round(latest.hit_ratio * 100)}%`,
            stored: formatBytes(latest.stored_bytes),
            evictions: latest.evictions,
          })}
        </p>
      ) : null}
      {data.length < 2 ? (
        <Empty>{t("One more reading is needed before the chart can show a change.")}</Empty>
      ) : (
        <div className="grid gap-4 lg:grid-cols-2">
          <Chart
            title={t("Hit ratio")}
            data={data}
            unit="percent"
            series={[{ key: "ratio", name: t("Hit ratio"), color: COLORS[0] ?? "var(--pine)" }]}
          />
          <Chart
            title={t("Memory")}
            data={data}
            unit="bytes"
            series={[
              { key: "stored", name: t("Stored"), color: COLORS[0] ?? "var(--pine)" },
              { key: "budget", name: t("Budget"), color: COLORS[1] ?? "var(--amber)" },
            ]}
          />
        </div>
      )}
    </section>
  );
}

function Chart({
  title,
  data,
  unit,
  series,
}: {
  title: string;
  data: Array<Record<string, number | null>>;
  unit: "percent" | "bytes";
  series: Array<{ key: string; name: string; color: string }>;
}) {
  return (
    <div>
      <p className="mb-1 text-sm text-muted">{title}</p>
      <div className="h-48">
        <ResponsiveContainer width="100%" height="100%">
          <AreaChart data={data}>
            <CartesianGrid stroke="var(--line)" vertical={false} />
            <XAxis
              dataKey="time"
              type="number"
              domain={["dataMin", "dataMax"]}
              tick={TICK}
              axisLine={AXIS_LINE}
              tickFormatter={(value: number) =>
                new Date(value).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
              }
            />
            <YAxis
              tick={TICK}
              axisLine={AXIS_LINE}
              width={56}
              tickFormatter={(value: number) => formatValue(value, unit)}
            />
            <Tooltip
              {...TOOLTIP_STYLES}
              labelFormatter={(value) => new Date(Number(value)).toLocaleString()}
              formatter={(value, name) => [formatValue(Number(value), unit), name]}
            />
            <Legend />
            {series.map((item) => (
              <Area
                key={item.key}
                type="monotone"
                dataKey={item.key}
                name={item.name}
                stroke={item.color}
                fill={item.color}
                fillOpacity={0.15}
                connectNulls
              />
            ))}
          </AreaChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
