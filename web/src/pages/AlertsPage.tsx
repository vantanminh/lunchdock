import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { useAuth } from "../auth";
import { Badge, Button, Card, Empty, Field, Select, TextInput } from "../components/ui";
import { errorMessage, formatTime } from "../format";
import type { Schemas } from "../types";

type Alert = Schemas["AlertView"];
type Channel = Schemas["ChannelView"];
type AlertKind = Schemas["AlertKindView"];

const CHANNEL_KINDS = ["telegram", "discord", "slack", "webhook", "smtp"] as const;
type ChannelKind = (typeof CHANNEL_KINDS)[number];

const CHANNEL_FIELDS: Record<
  ChannelKind,
  { key: string; label: string; secret?: boolean; optional?: boolean; placeholder?: string }[]
> = {
  telegram: [
    { key: "bot_token", label: "Bot token", secret: true },
    { key: "chat_id", label: "Chat ID", placeholder: "-1001234567890" },
  ],
  discord: [
    {
      key: "webhook_url",
      label: "Webhook URL",
      secret: true,
      placeholder: "https://discord.com/api/webhooks/…",
    },
  ],
  slack: [
    {
      key: "webhook_url",
      label: "Webhook URL",
      secret: true,
      placeholder: "https://hooks.slack.com/services/…",
    },
  ],
  webhook: [
    { key: "url", label: "URL", placeholder: "https://example.com/rudder" },
    { key: "secret", label: "Signing secret (HMAC-SHA256)", secret: true, optional: true },
  ],
  smtp: [
    { key: "host", label: "SMTP host", placeholder: "smtp.example.com" },
    { key: "port", label: "Port", optional: true, placeholder: "587" },
    { key: "username", label: "Username", optional: true },
    { key: "password", label: "Password", secret: true, optional: true },
    { key: "from", label: "From", placeholder: "Rudder <alerts@example.com>" },
    { key: "to", label: "To (comma separated)", placeholder: "ops@example.com" },
  ],
};

const SEVERITIES = ["info", "warning", "critical"] as const;

const severityTone: Record<string, string> = {
  info: "ok",
  warning: "partial",
  critical: "failed",
  firing: "failed",
  pending: "partial",
};

function channelConfig(kind: ChannelKind, values: Record<string, string>, security: string) {
  const config: Record<string, unknown> = { kind };
  for (const field of CHANNEL_FIELDS[kind]) {
    const value = (values[field.key] ?? "").trim();
    if (field.key === "port") {
      if (value) config.port = Number(value);
    } else if (field.key === "to") {
      config.to = value
        .split(",")
        .map((item) => item.trim())
        .filter(Boolean);
    } else if (value || field.secret) {
      config[field.key] = value;
    }
  }
  if (kind === "smtp") config.security = security;
  return config;
}

function ChannelsCard({ channels }: { channels: Channel[] }) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [kind, setKind] = useState<ChannelKind>("telegram");
  const [values, setValues] = useState<Record<string, string>>({});
  const [security, setSecurity] = useState("starttls");
  const [editing, setEditing] = useState<Channel | null>(null);

  const reset = () => {
    setEditing(null);
    setName("");
    setValues({});
  };
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["notification-channels"] });

  const save = useMutation({
    mutationFn: () => {
      const body = JSON.stringify({ name, config: channelConfig(kind, values, security) });
      return editing
        ? api<Channel>(`/api/v1/notification-channels/${editing.id}`, { method: "PUT", body })
        : api<Channel>("/api/v1/notification-channels", { method: "POST", body });
    },
    onSuccess: async () => {
      toast.success(editing ? "Channel updated" : "Channel added");
      reset();
      await refresh();
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save channel")),
  });
  const test = useMutation({
    mutationFn: (id: string) =>
      api<Schemas["TestResult"]>(`/api/v1/notification-channels/${id}/test`, { method: "POST" }),
    onSuccess: (result) =>
      result.ok ? toast.success(result.message) : toast.error(result.message),
    onError: (error) => toast.error(errorMessage(error, "Test failed")),
  });
  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/notification-channels/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success("Channel removed");
      await refresh();
      await queryClient.invalidateQueries({ queryKey: ["alerts"] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not remove channel")),
  });

  return (
    <Card title="Notification channels">
      <form
        className="grid gap-3 md:grid-cols-3"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field label="Name">
          <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <Field label="Kind">
          <Select
            value={kind}
            disabled={editing !== null}
            onChange={(event) => {
              setKind(event.target.value as ChannelKind);
              setValues({});
            }}
          >
            {CHANNEL_KINDS.map((item) => (
              <option key={item} value={item}>
                {item}
              </option>
            ))}
          </Select>
        </Field>
        {kind === "smtp" ? (
          <Field label="Security">
            <Select value={security} onChange={(event) => setSecurity(event.target.value)}>
              <option value="starttls">STARTTLS</option>
              <option value="tls">Implicit TLS</option>
              <option value="none">None (local relay only)</option>
            </Select>
          </Field>
        ) : null}
        {CHANNEL_FIELDS[kind].map((field) => (
          <Field
            key={field.key}
            label={field.secret && editing ? `${field.label} (leave empty to keep)` : field.label}
          >
            <TextInput
              required={!field.optional && !(field.secret && editing)}
              type={field.secret ? "password" : "text"}
              autoComplete="off"
              placeholder={field.placeholder}
              value={values[field.key] ?? ""}
              onChange={(event) => setValues({ ...values, [field.key]: event.target.value })}
            />
          </Field>
        ))}
        <div className="flex items-end gap-2">
          <Button type="submit" disabled={save.isPending}>
            {editing ? "Save" : "Add channel"}
          </Button>
          {editing ? (
            <Button quiet type="button" onClick={reset}>
              Cancel
            </Button>
          ) : null}
        </div>
      </form>
      <ul className="mt-4 divide-y divide-line text-sm">
        {channels.map((channel) => (
          <li key={channel.id} className="flex flex-wrap items-center gap-3 py-3">
            <div className="min-w-0 flex-1">
              <p className="font-medium">
                {channel.name} <span className="text-muted">{channel.kind}</span>
              </p>
              <p className="truncate text-muted">{channel.summary}</p>
            </div>
            <Button quiet onClick={() => test.mutate(channel.id)} disabled={test.isPending}>
              Send test
            </Button>
            <Button
              quiet
              onClick={() => {
                setEditing(channel);
                setName(channel.name);
                setKind(channel.kind as ChannelKind);
                setValues({});
              }}
            >
              Edit
            </Button>
            <Button quiet onClick={() => remove.mutate(channel.id)}>
              Remove
            </Button>
          </li>
        ))}
      </ul>
      {channels.length === 0 ? <Empty>No channels yet.</Empty> : null}
    </Card>
  );
}

const blankRule = {
  name: "",
  kind: "server_cpu",
  target: "",
  threshold: "90",
  duration: "300",
  events: "deploy.failed, backup.failed, build.failed",
  minSeverity: "warning",
};

function RulesCard({
  alerts,
  channels,
  kinds,
}: {
  alerts: Alert[];
  channels: Channel[];
  kinds: AlertKind[];
}) {
  const queryClient = useQueryClient();
  const servers = useQuery({
    queryKey: ["servers"],
    queryFn: () => api<Schemas["ServerView"][]>("/api/v1/servers"),
  });
  const services = useQuery({
    queryKey: ["services-all"],
    queryFn: () => api<Schemas["ServiceSummary"][]>("/api/v1/services"),
  });
  const [form, setForm] = useState(blankRule);
  const [picked, setPicked] = useState<string[]>([]);
  const [editing, setEditing] = useState<string | null>(null);
  const kind = kinds.find((item) => item.kind === form.kind);
  const scope = kind?.target ?? "server";

  const reset = () => {
    setForm(blankRule);
    setPicked([]);
    setEditing(null);
  };
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["alerts"] });

  const save = useMutation({
    mutationFn: () => {
      const target_spec =
        scope === "events"
          ? {
              events: form.events
                .split(",")
                .map((item) => item.trim())
                .filter(Boolean),
              min_severity: form.minSeverity,
            }
          : { [scope === "server" ? "server_id" : "service_id"]: form.target };
      const body = JSON.stringify({
        name: form.name,
        kind: form.kind,
        target_spec,
        threshold: kind?.needs_threshold ? Number(form.threshold) : null,
        duration_secs: scope === "events" ? null : Number(form.duration),
        channel_ids: picked,
        enabled: true,
      });
      return editing
        ? api<Alert>(`/api/v1/alerts/${editing}`, { method: "PUT", body })
        : api<Alert>("/api/v1/alerts", { method: "POST", body });
    },
    onSuccess: async () => {
      toast.success(editing ? "Rule updated" : "Rule added");
      reset();
      await refresh();
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save rule")),
  });
  const toggle = useMutation({
    mutationFn: (alert: Alert) =>
      api<Alert>(`/api/v1/alerts/${alert.id}`, {
        method: "PUT",
        body: JSON.stringify({
          name: alert.name,
          kind: alert.kind,
          target_spec: alert.target_spec,
          threshold: alert.threshold ?? null,
          duration_secs: alert.duration_secs,
          channel_ids: alert.channel_ids,
          enabled: !alert.enabled,
        }),
      }),
    onSuccess: refresh,
    onError: (error) => toast.error(errorMessage(error, "Could not update rule")),
  });
  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/alerts/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success("Rule removed");
      await refresh();
    },
    onError: (error) => toast.error(errorMessage(error, "Could not remove rule")),
  });

  function edit(alert: Alert) {
    const spec = alert.target_spec as Record<string, unknown>;
    setEditing(alert.id);
    setPicked(alert.channel_ids);
    setForm({
      name: alert.name,
      kind: alert.kind,
      target: String(spec.server_id ?? spec.service_id ?? ""),
      threshold: String(alert.threshold ?? ""),
      duration: String(alert.duration_secs),
      events: Array.isArray(spec.events) ? spec.events.join(", ") : blankRule.events,
      minSeverity: String(spec.min_severity ?? "warning"),
    });
  }

  function describeTarget(alert: Alert): string {
    const spec = alert.target_spec as Record<string, unknown>;
    if (Array.isArray(spec.events)) {
      return `events ${spec.events.join(", ")} at ${String(spec.min_severity ?? "info")}+`;
    }
    if (spec.server_id) {
      return `server ${servers.data?.find((s) => s.id === spec.server_id)?.name ?? "?"}`;
    }
    const service = services.data?.find((s) => s.id === spec.service_id);
    return service ? `service ${service.project_name}/${service.name}` : "service ?";
  }

  const set = (key: keyof typeof blankRule) => (event: { target: { value: string } }) =>
    setForm({ ...form, [key]: event.target.value });

  return (
    <Card title="Alert rules">
      <form
        className="grid gap-3 md:grid-cols-3"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field label="Name">
          <TextInput required value={form.name} onChange={set("name")} />
        </Field>
        <Field label="Condition">
          <Select
            value={form.kind}
            onChange={(event) => setForm({ ...form, kind: event.target.value, target: "" })}
          >
            {kinds.map((item) => (
              <option key={item.kind} value={item.kind}>
                {item.description}
              </option>
            ))}
          </Select>
        </Field>
        {scope === "server" ? (
          <Field label="Server">
            <Select required value={form.target} onChange={set("target")}>
              <option value="">Choose a server</option>
              {(servers.data ?? []).map((server) => (
                <option key={server.id} value={server.id}>
                  {server.name}
                </option>
              ))}
            </Select>
          </Field>
        ) : null}
        {scope === "service" ? (
          <Field label="Service">
            <Select required value={form.target} onChange={set("target")}>
              <option value="">Choose a service</option>
              {(services.data ?? []).map((service) => (
                <option key={service.id} value={service.id}>
                  {service.project_name}/{service.environment_name}/{service.name}
                </option>
              ))}
            </Select>
          </Field>
        ) : null}
        {scope === "events" ? (
          <>
            <Field label="Event patterns">
              <TextInput required value={form.events} onChange={set("events")} />
            </Field>
            <Field label="Minimum severity">
              <Select value={form.minSeverity} onChange={set("minSeverity")}>
                {SEVERITIES.map((item) => (
                  <option key={item} value={item}>
                    {item}
                  </option>
                ))}
              </Select>
            </Field>
          </>
        ) : null}
        {kind?.needs_threshold ? (
          <Field
            label={
              form.kind === "service_memory"
                ? "Threshold (MiB)"
                : form.kind === "http_errors"
                  ? "Threshold (5xx responses)"
                  : "Threshold (%)"
            }
          >
            <TextInput
              required
              type="number"
              min="0"
              step="any"
              value={form.threshold}
              onChange={set("threshold")}
            />
          </Field>
        ) : null}
        {scope !== "events" ? (
          <Field label="For at least (seconds)">
            <TextInput
              required
              type="number"
              min="0"
              value={form.duration}
              onChange={set("duration")}
            />
          </Field>
        ) : null}
        <fieldset className="space-y-1 text-sm md:col-span-3">
          <legend className="font-medium">Notify</legend>
          {channels.length === 0 ? <Empty>Add a channel below first.</Empty> : null}
          <div className="flex flex-wrap gap-4">
            {channels.map((channel) => (
              <label key={channel.id} className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={picked.includes(channel.id)}
                  onChange={(event) =>
                    setPicked(
                      event.target.checked
                        ? [...picked, channel.id]
                        : picked.filter((id) => id !== channel.id),
                    )
                  }
                />
                {channel.name} <span className="text-muted">{channel.kind}</span>
              </label>
            ))}
          </div>
        </fieldset>
        <div className="flex items-end gap-2">
          <Button type="submit" disabled={save.isPending}>
            {editing ? "Save rule" : "Add rule"}
          </Button>
          {editing ? (
            <Button quiet type="button" onClick={reset}>
              Cancel
            </Button>
          ) : null}
        </div>
      </form>
      <ul className="mt-4 divide-y divide-line text-sm">
        {alerts.map((alert) => (
          <li key={alert.id} className="flex flex-wrap items-center gap-3 py-3">
            <div className="min-w-0 flex-1">
              <p className="flex items-center gap-2 font-medium">
                {alert.name}
                <Badge
                  value={alert.enabled ? alert.state : "disabled"}
                  tone={alert.enabled ? severityTone[alert.state] : "stopped"}
                />
              </p>
              <p className="truncate text-muted">
                {alert.kind} · {describeTarget(alert)}
                {alert.threshold != null ? ` · threshold ${alert.threshold}` : ""}
                {alert.last_value != null ? ` · last ${alert.last_value.toFixed(1)}` : ""}
                {alert.last_fired_at ? ` · fired ${formatTime(alert.last_fired_at)}` : ""}
              </p>
            </div>
            <Button quiet onClick={() => toggle.mutate(alert)}>
              {alert.enabled ? "Disable" : "Enable"}
            </Button>
            <Button quiet onClick={() => edit(alert)}>
              Edit
            </Button>
            <Button quiet onClick={() => remove.mutate(alert.id)}>
              Remove
            </Button>
          </li>
        ))}
      </ul>
      {alerts.length === 0 ? <Empty>No rules yet.</Empty> : null}
    </Card>
  );
}

function EventsCard() {
  const events = useQuery({
    queryKey: ["events"],
    queryFn: () => api<Schemas["EventView"][]>("/api/v1/events?limit=50"),
    refetchInterval: 15_000,
  });
  return (
    <Card title="Recent events">
      <ul className="divide-y divide-line text-sm">
        {(events.data ?? []).map((event) => (
          <li key={event.id} className="flex flex-wrap items-start gap-3 py-2">
            <Badge value={event.severity} tone={severityTone[event.severity]} />
            <div className="min-w-0 flex-1">
              <p className="font-medium">{event.title}</p>
              {event.message ? <p className="truncate text-muted">{event.message}</p> : null}
            </div>
            <div className="text-right text-xs text-muted">
              <p>{event.kind}</p>
              <p>{formatTime(event.created_at)}</p>
            </div>
          </li>
        ))}
      </ul>
      {events.data?.length === 0 ? <Empty>No events yet.</Empty> : null}
    </Card>
  );
}

export function AlertsPage() {
  const { user } = useAuth();
  const alerts = useQuery({
    queryKey: ["alerts"],
    queryFn: () => api<Alert[]>("/api/v1/alerts"),
    refetchInterval: 10_000,
  });
  const kinds = useQuery({
    queryKey: ["alert-kinds"],
    queryFn: () => api<AlertKind[]>("/api/v1/alert-kinds"),
  });
  const channels = useQuery({
    queryKey: ["notification-channels"],
    queryFn: () => api<Channel[]>("/api/v1/notification-channels"),
    enabled: user?.role === "admin",
  });

  return (
    <>
      <h1 className="text-2xl font-semibold">Alerts</h1>
      <p className="text-sm text-muted">
        Rules are checked every 10 seconds. Notifications go to Telegram, Discord, Slack, a signed
        webhook, or email, and are retried up to five times.
      </p>
      <RulesCard
        alerts={alerts.data ?? []}
        channels={channels.data ?? []}
        kinds={kinds.data ?? []}
      />
      {user?.role === "admin" ? <ChannelsCard channels={channels.data ?? []} /> : null}
      <EventsCard />
    </>
  );
}
