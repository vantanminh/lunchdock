import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { useAuth } from "../auth";
import {
  Badge,
  Button,
  Card,
  Empty,
  Field,
  PickField,
  QuickPick,
  Select,
  TextInput,
} from "../components/ui";
import { errorMessage, formatTime } from "../format";
import { msg, useT } from "../i18n";
import {
  ALERT_NAMES,
  alertThresholds,
  EVENT_SETS,
  pickDurations,
  SEVERITIES as SEVERITY_CHOICES,
  SMTP_PORTS,
  SMTP_PROVIDERS,
} from "../presets";
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
    { key: "bot_token", label: msg("Bot token"), secret: true },
    { key: "chat_id", label: msg("Chat ID"), placeholder: "-1001234567890" },
  ],
  discord: [
    {
      key: "webhook_url",
      label: msg("Webhook URL"),
      secret: true,
      placeholder: "https://discord.com/api/webhooks/…",
    },
  ],
  slack: [
    {
      key: "webhook_url",
      label: msg("Webhook URL"),
      secret: true,
      placeholder: "https://hooks.slack.com/services/…",
    },
  ],
  webhook: [
    { key: "url", label: msg("URL"), placeholder: "https://example.com/rudder" },
    { key: "secret", label: msg("Signing secret (HMAC-SHA256)"), secret: true, optional: true },
  ],
  smtp: [
    { key: "host", label: msg("SMTP host"), placeholder: "smtp.example.com" },
    { key: "port", label: msg("Port"), optional: true, placeholder: "587" },
    { key: "username", label: msg("Username"), optional: true },
    { key: "password", label: msg("Password"), secret: true, optional: true },
    { key: "from", label: msg("From"), placeholder: "Rudder <alerts@example.com>" },
    { key: "to", label: msg("To (comma separated)"), placeholder: "ops@example.com" },
  ],
};

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

const CHANNEL_NAMES: Record<ChannelKind, string> = {
  telegram: "Telegram",
  discord: "Discord",
  slack: "Slack",
  webhook: "Webhook",
  smtp: "Email",
};

function ChannelsCard({ channels }: { channels: Channel[] }) {
  const t = useT();
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
      toast.success(editing ? t("Channel updated") : t("Channel added"));
      reset();
      await refresh();
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not save channel"))),
  });
  const test = useMutation({
    mutationFn: (id: string) =>
      api<Schemas["TestResult"]>(`/api/v1/notification-channels/${id}/test`, { method: "POST" }),
    onSuccess: (result) =>
      result.ok ? toast.success(result.message) : toast.error(result.message),
    onError: (error) => toast.error(errorMessage(error, t("Test failed"))),
  });
  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/notification-channels/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success(t("Channel removed"));
      await refresh();
      await queryClient.invalidateQueries({ queryKey: ["alerts"] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not remove channel"))),
  });

  return (
    <Card title={t("Notification channels")}>
      <form
        className="grid gap-3 md:grid-cols-3"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field label={t("Name")}>
          <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <div className="md:col-span-2">
          <PickField
            label={t("Kind")}
            options={CHANNEL_KINDS.map((item) => ({ label: CHANNEL_NAMES[item], value: item }))}
            value={kind}
            onPick={(next) => {
              if (editing !== null) return;
              const previous = CHANNEL_NAMES[kind];
              setKind(next as ChannelKind);
              setValues({});
              if (!name || name === previous) setName(CHANNEL_NAMES[next as ChannelKind]);
            }}
          />
        </div>
        {kind === "smtp" ? (
          <>
            <div className="md:col-span-3">
              <PickField
                label={t("Mail provider")}
                hint={t(
                  "Fills in the host, port, and security. Add your own username and password.",
                )}
                options={SMTP_PROVIDERS.map((item) => ({ label: item.label, value: item.label }))}
                value={SMTP_PROVIDERS.find((item) => item.host === values.host)?.label ?? ""}
                onPick={(label) => {
                  const provider = SMTP_PROVIDERS.find((item) => item.label === label);
                  if (!provider) return;
                  setValues({
                    ...values,
                    host: provider.host,
                    port: provider.port,
                    ...(provider.username && !values.username
                      ? { username: provider.username }
                      : {}),
                  });
                  setSecurity(provider.security);
                }}
              />
            </div>
            <PickField
              label={t("Security")}
              options={SMTP_SECURITY}
              value={security}
              onPick={setSecurity}
            />
          </>
        ) : null}
        {CHANNEL_FIELDS[kind].map((field) => (
          <Field
            key={field.key}
            label={
              field.secret && editing
                ? t("{label} (leave empty to keep)", { label: t(field.label) })
                : t(field.label)
            }
            presets={
              kind === "smtp" && field.key === "port" ? (
                <QuickPick
                  label={t("Port presets")}
                  options={SMTP_PORTS}
                  value={values.port}
                  onPick={(port) => {
                    setValues({ ...values, port });
                    setSecurity(port === "465" ? "tls" : port === "25" ? "none" : "starttls");
                  }}
                />
              ) : undefined
            }
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
            {editing ? t("Save") : t("Add channel")}
          </Button>
          {editing ? (
            <Button quiet type="button" onClick={reset}>
              {t("Cancel")}
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
              {t("Send test")}
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
              {t("Edit")}
            </Button>
            <Button quiet onClick={() => remove.mutate(channel.id)}>
              {t("Remove")}
            </Button>
          </li>
        ))}
      </ul>
      {channels.length === 0 ? <Empty>{t("No channels yet.")}</Empty> : null}
    </Card>
  );
}

const SMTP_SECURITY = [
  { label: "STARTTLS", value: "starttls", hint: msg("Upgrade a plain connection; port 587") },
  { label: msg("Implicit TLS"), value: "tls", hint: msg("Encrypted from the start; port 465") },
  {
    label: msg("None (local relay only)"),
    value: "none",
    hint: msg("Unencrypted; only for a relay on this network"),
  },
];

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
  const t = useT();
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
  const suggestedName = ALERT_NAMES[form.kind];

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
      toast.success(editing ? t("Rule updated") : t("Rule added"));
      reset();
      await refresh();
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not save rule"))),
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
    onError: (error) => toast.error(errorMessage(error, t("Could not update rule"))),
  });
  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/alerts/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success(t("Rule removed"));
      await refresh();
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not remove rule"))),
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
    <Card title={t("Alert rules")}>
      <form
        className="grid gap-3 md:grid-cols-3"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field
          label={t("Name")}
          presets={
            suggestedName ? (
              <QuickPick
                label={t("Name suggestions")}
                options={[{ label: suggestedName, value: t(suggestedName) }]}
                value={form.name}
                onPick={(name) => setForm({ ...form, name })}
              />
            ) : undefined
          }
        >
          <TextInput required value={form.name} onChange={set("name")} />
        </Field>
        <Field label={t("Condition")}>
          <Select
            value={form.kind}
            onChange={(event) => setForm({ ...form, kind: event.target.value, target: "" })}
          >
            {kinds.map((item) => (
              <option key={item.kind} value={item.kind}>
                {t(item.description)}
              </option>
            ))}
          </Select>
        </Field>
        {scope === "server" ? (
          <Field label={t("Server")}>
            <Select required value={form.target} onChange={set("target")}>
              <option value="">{t("Choose a server")}</option>
              {(servers.data ?? []).map((server) => (
                <option key={server.id} value={server.id}>
                  {server.name}
                </option>
              ))}
            </Select>
          </Field>
        ) : null}
        {scope === "service" ? (
          <Field label={t("Service")}>
            <Select required value={form.target} onChange={set("target")}>
              <option value="">{t("Choose a service")}</option>
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
            <Field
              label={t("Event patterns")}
              hint={t("Comma separated. Use * as a wildcard, for example backup.*")}
              presets={
                <QuickPick
                  label={t("Event sets")}
                  options={EVENT_SETS}
                  value={form.events}
                  onPick={(events) => setForm({ ...form, events })}
                />
              }
            >
              <TextInput required value={form.events} onChange={set("events")} />
            </Field>
            <PickField
              label={t("Minimum severity")}
              options={SEVERITY_CHOICES}
              value={form.minSeverity}
              onPick={(minSeverity) => setForm({ ...form, minSeverity })}
            />
          </>
        ) : null}
        {kind?.needs_threshold ? (
          <Field
            label={
              form.kind === "service_memory"
                ? t("Threshold (MiB)")
                : form.kind === "http_errors"
                  ? t("Threshold (5xx responses)")
                  : t("Threshold (%)")
            }
            presets={
              <QuickPick
                label={t("Threshold presets")}
                options={alertThresholds(form.kind)}
                value={form.threshold}
                onPick={(threshold) => setForm({ ...form, threshold })}
              />
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
          <Field
            label={t("For at least (seconds)")}
            presets={
              <QuickPick
                label={t("Duration presets")}
                options={pickDurations("60", "300", "900", "1800", "3600")}
                value={form.duration}
                onPick={(duration) => setForm({ ...form, duration })}
              />
            }
          >
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
          <legend className="font-medium">{t("Notify")}</legend>
          {channels.length === 0 ? <Empty>{t("Add a channel below first.")}</Empty> : null}
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
            {editing ? t("Save rule") : t("Add rule")}
          </Button>
          {editing ? (
            <Button quiet type="button" onClick={reset}>
              {t("Cancel")}
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
                {alert.threshold != null
                  ? ` · ${t("threshold {value}", { value: alert.threshold })}`
                  : ""}
                {alert.last_value != null
                  ? ` · ${t("last {value}", { value: alert.last_value.toFixed(1) })}`
                  : ""}
                {alert.last_fired_at
                  ? ` · ${t("fired {time}", { time: formatTime(alert.last_fired_at) })}`
                  : ""}
              </p>
            </div>
            <Button quiet onClick={() => toggle.mutate(alert)}>
              {alert.enabled ? t("Disable") : t("Enable")}
            </Button>
            <Button quiet onClick={() => edit(alert)}>
              {t("Edit")}
            </Button>
            <Button quiet onClick={() => remove.mutate(alert.id)}>
              {t("Remove")}
            </Button>
          </li>
        ))}
      </ul>
      {alerts.length === 0 ? <Empty>{t("No rules yet.")}</Empty> : null}
    </Card>
  );
}

function EventsCard() {
  const t = useT();
  const events = useQuery({
    queryKey: ["events"],
    queryFn: () => api<Schemas["EventView"][]>("/api/v1/events?limit=50"),
    refetchInterval: 15_000,
  });
  return (
    <Card title={t("Recent events")}>
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
      {events.data?.length === 0 ? <Empty>{t("No events yet.")}</Empty> : null}
    </Card>
  );
}

export function AlertsPage() {
  const t = useT();
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
      <h1 className="text-2xl font-semibold">{t("Alerts")}</h1>
      <p className="text-sm text-muted">
        {t(
          "Rules are checked every 10 seconds. Notifications go to Telegram, Discord, Slack, a signed webhook, or email, and are retried up to five times.",
        )}
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
