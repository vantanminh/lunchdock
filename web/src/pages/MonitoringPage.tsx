import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { api } from "../api";
import { useAuth } from "../auth";
import { formatValue, MetricsGrid, SERVER_CHARTS } from "../components/metrics";
import { Badge, Button, Card, Empty, Field, QuickPick, Select, TextInput } from "../components/ui";
import { errorMessage, formatTime } from "../format";
import { useT } from "../i18n";
import { METRIC_RETENTION } from "../presets";
import type { Schemas } from "../types";

type MetricsRetention = Schemas["MetricsRetention"];

const RETENTION_FIELDS: [keyof MetricsRetention, string, number][] = [
  ["raw_hours", "10-second samples (hours)", 168],
  ["m1_days", "1-minute rollups (days)", 365],
  ["m5_days", "5-minute rollups (days)", 365],
  ["h1_days", "Hourly rollups (days)", 365],
];

const retentionKey = (values: Record<string, string>) =>
  RETENTION_FIELDS.map(([key]) => values[key] ?? "").join("|");

function MetricsRetentionCard() {
  const t = useT();
  const queryClient = useQueryClient();
  const retention = useQuery({
    queryKey: ["retention"],
    queryFn: () => api<Schemas["RetentionSettings"]>("/api/v1/settings/retention"),
  });
  const [values, setValues] = useState<Record<string, string>>({});
  useEffect(() => {
    if (retention.data) {
      setValues(
        Object.fromEntries(
          Object.entries(retention.data.metrics).map(([key, value]) => [key, String(value)]),
        ),
      );
    }
  }, [retention.data]);
  const save = useMutation({
    mutationFn: () =>
      api("/api/v1/settings/retention", {
        method: "PUT",
        body: JSON.stringify({
          metrics: Object.fromEntries(RETENTION_FIELDS.map(([key]) => [key, Number(values[key])])),
        }),
      }),
    onSuccess: async () => {
      toast.success("Retention saved");
      await queryClient.invalidateQueries({ queryKey: ["retention"] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save retention")),
  });
  return (
    <Card title="Metric retention">
      <form
        className="grid gap-3 md:grid-cols-4"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <div className="space-y-1.5 md:col-span-4">
          <span className="text-[13px] font-medium text-ink-soft">{t("Preset")}</span>
          <QuickPick
            label={t("Metric retention presets")}
            options={METRIC_RETENTION.map((preset) => ({
              label: preset.label,
              value: retentionKey(preset.values),
              hint: preset.hint,
            }))}
            value={retentionKey(values)}
            onPick={(_, option) => {
              const preset = METRIC_RETENTION.find((item) => item.label === option.label);
              if (preset) setValues({ ...values, ...preset.values });
            }}
          />
        </div>
        {RETENTION_FIELDS.map(([key, label, max]) => (
          <Field key={key} label={label}>
            <TextInput
              type="number"
              min={1}
              max={max}
              value={values[key] ?? ""}
              onChange={(event) => setValues({ ...values, [key]: event.target.value })}
            />
          </Field>
        ))}
        <div className="md:col-span-4">
          <Button type="submit" disabled={save.isPending || !retention.data}>
            Save retention
          </Button>
        </div>
      </form>
    </Card>
  );
}

export function MonitoringPage() {
  const { user } = useAuth();
  const overview = useQuery({
    queryKey: ["metrics-overview"],
    queryFn: () => api<Schemas["MetricsOverview"]>("/api/v1/metrics/overview"),
    refetchInterval: 10_000,
  });
  const servers = overview.data?.servers ?? [];
  const services = [...(overview.data?.services ?? [])].sort(
    (a, b) => (b.cpu_percent ?? 0) - (a.cpu_percent ?? 0),
  );
  const [picked, setPicked] = useState<string | null>(null);
  const serverId = picked ?? servers[0]?.server_id ?? null;
  const serverName = (id: string) => servers.find((s) => s.server_id === id)?.name ?? "—";

  return (
    <>
      <h1 className="text-2xl font-semibold">Monitoring</h1>
      <p className="text-sm text-muted">
        Sampled every 10 seconds, rolled up to 1-minute, 5-minute, and hourly averages. Configure
        rules and channels under <Link to="/alerts">Alerts</Link>.
      </p>
      <Card title="Servers">
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead className="text-muted">
              <tr>
                <th className="py-2 pr-3 font-medium">Server</th>
                <th className="py-2 pr-3 font-medium">Status</th>
                <th className="py-2 pr-3 font-medium">CPU</th>
                <th className="py-2 pr-3 font-medium">Memory</th>
                <th className="py-2 pr-3 font-medium">Disk</th>
                <th className="py-2 pr-3 font-medium">Load</th>
                <th className="py-2 font-medium">Sampled</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {servers.map((server) => (
                <tr
                  key={server.server_id}
                  className={`cursor-pointer ${server.server_id === serverId ? "bg-paper" : ""}`}
                  onClick={() => setPicked(server.server_id)}
                >
                  <td className="py-2 pr-3 font-medium">{server.name}</td>
                  <td className="py-2 pr-3">
                    <Badge value={server.status} />
                  </td>
                  <td className="py-2 pr-3">{formatValue(server.cpu_percent, "percent")}</td>
                  <td className="py-2 pr-3">{formatValue(server.memory_percent, "percent")}</td>
                  <td className="py-2 pr-3">{formatValue(server.disk_percent, "percent")}</td>
                  <td className="py-2 pr-3">{formatValue(server.load1, "plain")}</td>
                  <td className="py-2 text-muted">{formatTime(server.sampled_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {servers.length === 0 && !overview.isLoading ? <Empty>No servers yet.</Empty> : null}
      </Card>
      {serverId ? (
        <Card title={`Server ${serverName(serverId)}`}>
          {servers.length > 1 ? (
            <div className="mb-3 max-w-xs">
              <Field label="Server">
                <Select value={serverId} onChange={(event) => setPicked(event.target.value)}>
                  {servers.map((server) => (
                    <option key={server.server_id} value={server.server_id}>
                      {server.name}
                    </option>
                  ))}
                </Select>
              </Field>
            </div>
          ) : null}
          <MetricsGrid charts={SERVER_CHARTS} target={{ server_id: serverId }} />
        </Card>
      ) : null}
      <Card title="Services">
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead className="text-muted">
              <tr>
                <th className="py-2 pr-3 font-medium">Service</th>
                <th className="py-2 pr-3 font-medium">Server</th>
                <th className="py-2 pr-3 font-medium">CPU</th>
                <th className="py-2 pr-3 font-medium">Memory</th>
                <th className="py-2 font-medium">Containers up</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {services.map((service) => (
                <tr key={service.service_id}>
                  <td className="py-2 pr-3 font-medium">
                    <Link to={`/services/${service.service_id}`}>{service.name}</Link>
                  </td>
                  <td className="py-2 pr-3 text-muted">{serverName(service.server_id)}</td>
                  <td className="py-2 pr-3">{formatValue(service.cpu_percent, "percent")}</td>
                  <td className="py-2 pr-3">{formatValue(service.memory_bytes, "bytes")}</td>
                  <td className="py-2">{formatValue(service.containers, "count")}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {services.length === 0 && !overview.isLoading ? (
          <Empty>No running services have been sampled yet.</Empty>
        ) : null}
      </Card>
      {user?.role === "admin" ? <MetricsRetentionCard /> : null}
    </>
  );
}
