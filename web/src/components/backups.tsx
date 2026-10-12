import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { api } from "../api";
import { errorMessage, formatBytes, formatDuration, formatTime } from "../format";
import { useT } from "../i18n";
import {
  allTimezones,
  BANDWIDTH,
  compressionLevels,
  EXCLUDE_GLOBS,
  pickDurations,
  plain,
  RETENTION,
  retentionKey,
  RETRIES,
  SCHEDULES,
  timezoneOptions,
} from "../presets";
import type { Schemas } from "../types";
import { Icon } from "./icons";
import {
  Badge,
  Button,
  buttonClass,
  DataTable,
  Disclosure,
  Empty,
  Field,
  LogPre,
  QuickPick,
  Select,
  Td,
  TextArea,
  TextInput,
  Th,
} from "./ui";

type Job = Schemas["BackupJobView"];
type Run = Schemas["BackupRunView"];
type Destination = Schemas["DestinationView"];

interface Presets {
  schedules: { name: string; cron: string }[];
  filename_tokens: string[];
}

export interface BackupTarget {
  serviceId?: string;
  serverId?: string;
  isDatabase?: boolean;
  /** Back up the panel itself: its database with the master key, or the settings export. */
  panel?: boolean;
}

const lines = (value: string) =>
  value
    .split(/[\n,]/)
    .map((item) => item.trim())
    .filter(Boolean);

const words = (value: string) => value.split(/\s+/).filter(Boolean);

function useDestinations() {
  return useQuery({
    queryKey: ["s3-destinations"],
    queryFn: () => api<Destination[]>("/api/v1/s3-destinations"),
  });
}

function nameSuggestions(kind: string, isDatabase: boolean): string[] {
  if (kind === "panel_db") return ["panel-nightly", "panel-weekly"];
  if (kind === "panel_settings") return ["settings-export"];
  if (kind === "database" || isDatabase) return ["db-nightly", "db-hourly", "db-weekly"];
  return ["nightly", "hourly", "weekly", "monthly"];
}

export function BackupJobForm({ target, onSaved }: { target: BackupTarget; onSaved?: () => void }) {
  const t = useT();
  const queryClient = useQueryClient();
  const destinations = useDestinations();
  const presets = useQuery({
    queryKey: ["backup-presets"],
    queryFn: () => api<Presets>("/api/v1/backup-presets"),
  });
  const servers = useQuery({
    queryKey: ["servers"],
    queryFn: () => api<Schemas["ServerView"][]>("/api/v1/servers"),
    enabled: !target.serverId && !target.serviceId && !target.panel,
  });
  const [form, setForm] = useState({
    name: "",
    target_kind: target.panel ? "panel_db" : target.isDatabase ? "database" : "volume",
    include_history: false,
    server_id: target.serverId ?? "",
    target_value: "",
    schedule_cron: "@daily",
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    path_prefix: "rudder",
    filename_template: "{name}-{timestamp}",
    keep_last: "7",
    keep_days: "",
    daily: "",
    weekly: "",
    monthly: "",
    compression: "zstd",
    compression_level: "3",
    passphrase: "",
    dump_all: false,
    include_databases: "",
    exclude_databases: "",
    include_tables: "",
    exclude_tables: "",
    extra_args: "",
    include_globs: "",
    exclude_globs: "",
    stop_service: false,
    pre_hook: "",
    post_hook: "",
    timeout_secs: "3600",
    retries: "2",
    bandwidth_mbps: "",
    verify_after_upload: false,
    notify_on_success: false,
  });
  const [chosen, setChosen] = useState<string[]>([]);

  const save = useMutation({
    mutationFn: () => {
      const number = (value: string) => (value ? Number(value) : undefined);
      const targetSpec =
        form.target_kind === "volume"
          ? { volume: form.target_value }
          : form.target_kind === "bind"
            ? { source: form.target_value }
            : form.target_kind === "path"
              ? { path: form.target_value }
              : form.target_kind === "panel_db"
                ? { include_history: form.include_history }
                : {};
      return api<Job>("/api/v1/backup-jobs", {
        method: "POST",
        body: JSON.stringify({
          name: form.name,
          target_kind: form.target_kind,
          server_id: (!target.panel && form.server_id) || undefined,
          service_id: target.serviceId,
          target_spec: targetSpec,
          schedule_cron: form.schedule_cron,
          timezone: form.timezone,
          destination_ids: chosen,
          path_prefix: form.path_prefix,
          filename_template: form.filename_template,
          retention: {
            keep_last: number(form.keep_last),
            keep_days: number(form.keep_days),
            daily: number(form.daily),
            weekly: number(form.weekly),
            monthly: number(form.monthly),
          },
          compression: form.compression,
          compression_level: number(form.compression_level),
          encryption_passphrase: form.passphrase || undefined,
          tool_options: {
            dump_all: form.dump_all,
            include_databases: lines(form.include_databases),
            exclude_databases: lines(form.exclude_databases),
            include_tables: lines(form.include_tables),
            exclude_tables: lines(form.exclude_tables),
            extra_args: words(form.extra_args),
            include_globs: lines(form.include_globs),
            exclude_globs: lines(form.exclude_globs),
          },
          consistency: {
            stop_service: form.stop_service,
            pre_hook: words(form.pre_hook),
            post_hook: words(form.post_hook),
          },
          timeout_secs: number(form.timeout_secs),
          retries: number(form.retries),
          bandwidth_limit_bps: form.bandwidth_mbps
            ? Math.round(Number(form.bandwidth_mbps) * 125_000)
            : undefined,
          verify_after_upload: form.verify_after_upload,
          notify_on_success: form.notify_on_success,
        }),
      });
    },
    onSuccess: async () => {
      toast.success(t("Backup job saved"));
      setForm({ ...form, name: "", passphrase: "" });
      await queryClient.invalidateQueries({ queryKey: ["backup-jobs"] });
      onSaved?.();
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not save the backup job"))),
  });

  const set = (key: keyof typeof form) => (event: { target: { value: string } }) =>
    setForm({ ...form, [key]: event.target.value });
  const assign = (values: Partial<typeof form>) => setForm({ ...form, ...values });
  const appendLine = (key: "include_globs" | "exclude_globs", line: string) => {
    const lines = form[key].split("\n").filter(Boolean);
    if (!lines.includes(line)) assign({ [key]: [...lines, line].join("\n") });
  };
  const check = (key: keyof typeof form) => (event: { target: { checked: boolean } }) =>
    setForm({ ...form, [key]: event.target.checked });
  const isPanel = form.target_kind.startsWith("panel_");
  const isArchive = form.target_kind !== "database" && !isPanel;
  const presetNames = (presets.data?.schedules ?? []).map((preset) => preset.name);
  const detectedTimezone = useMemo(() => Intl.DateTimeFormat().resolvedOptions().timeZone, []);
  const timezones = useMemo(allTimezones, []);

  if (destinations.data?.length === 0) {
    return (
      <Empty>
        {t("Add an {link} before creating backup jobs.", {
          link: (
            <Link className="text-accent" to="/s3-destinations">
              {t("S3 destination")}
            </Link>
          ),
        })}
      </Empty>
    );
  }

  return (
    <form
      className="space-y-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (chosen.length === 0) {
          toast.error(t("Choose at least one destination"));
          return;
        }
        save.mutate();
      }}
    >
      <div className="grid gap-3 md:grid-cols-2">
        <Field
          label={t("Job name")}
          presets={
            <QuickPick
              label={t("Name suggestions")}
              options={plain(nameSuggestions(form.target_kind, Boolean(target.isDatabase)))}
              value={form.name}
              onPick={(name) => assign({ name })}
            />
          }
        >
          <TextInput required value={form.name} placeholder="nightly" onChange={set("name")} />
        </Field>
        <Field label={t("What to back up")}>
          <Select value={form.target_kind} onChange={set("target_kind")}>
            {target.panel ? (
              <>
                <option value="panel_db">{t("Panel database and master key")}</option>
                <option value="panel_settings">{t("Settings export (JSON, no secrets)")}</option>
              </>
            ) : (
              <>
                {target.isDatabase ? <option value="database">{t("Database dump")}</option> : null}
                <option value="volume">{t("Docker volume")}</option>
                <option value="bind">{t("Bind mount")}</option>
                <option value="path">{t("Host path")}</option>
              </>
            )}
          </Select>
        </Field>
        {!target.serverId && !target.serviceId && !target.panel ? (
          <Field label={t("Server")}>
            <Select required value={form.server_id} onChange={set("server_id")}>
              <option value="">{t("Choose")}</option>
              {(servers.data ?? []).map((server) => (
                <option key={server.id} value={server.id}>
                  {server.name}
                </option>
              ))}
            </Select>
          </Field>
        ) : null}
        {isArchive ? (
          <Field
            label={
              form.target_kind === "volume"
                ? t("Volume name")
                : form.target_kind === "bind"
                  ? t("Bind source on the host")
                  : t("Absolute host path")
            }
          >
            <TextInput
              required
              value={form.target_value}
              placeholder={form.target_kind === "volume" ? "app-data" : "/srv/app/uploads"}
              onChange={set("target_value")}
            />
          </Field>
        ) : null}
        <Field
          label={t("Schedule (cron or preset, empty = manual only)")}
          presets={
            <QuickPick
              label={t("Schedule presets")}
              options={SCHEDULES}
              value={form.schedule_cron}
              onPick={(schedule_cron) => assign({ schedule_cron })}
            />
          }
        >
          <TextInput
            list="backup-presets"
            value={form.schedule_cron}
            placeholder={t("Manual only")}
            onChange={set("schedule_cron")}
          />
          <datalist id="backup-presets">
            {presetNames.map((name) => (
              <option key={name} value={name} />
            ))}
          </datalist>
        </Field>
        <Field
          label={t("Timezone")}
          hint={t("Schedule times follow this timezone.")}
          presets={
            <QuickPick
              label={t("Timezone presets")}
              options={timezoneOptions(detectedTimezone)}
              value={form.timezone}
              onPick={(timezone) => assign({ timezone })}
            />
          }
        >
          <TextInput
            required
            list="backup-timezones"
            value={form.timezone}
            onChange={set("timezone")}
          />
          <datalist id="backup-timezones">
            {timezones.map((zone) => (
              <option key={zone} value={zone} />
            ))}
          </datalist>
        </Field>
        <Field
          label={t("Compression")}
          presets={
            <QuickPick
              label={t("Compression level presets")}
              options={compressionLevels(form.compression)}
              value={form.compression_level}
              onPick={(compression_level) => assign({ compression_level })}
            />
          }
        >
          <div className="flex gap-2">
            <Select
              value={form.compression}
              onChange={(event) =>
                assign({
                  compression: event.target.value,
                  compression_level: compressionLevels(event.target.value)[1]?.value ?? "",
                })
              }
            >
              <option value="zstd">zstd</option>
              <option value="gzip">gzip</option>
              <option value="none">{t("none")}</option>
            </Select>
            <TextInput
              aria-label={t("Compression level")}
              className="w-20"
              value={form.compression_level}
              disabled={form.compression === "none"}
              onChange={set("compression_level")}
            />
          </div>
        </Field>
        <Field
          label={
            form.target_kind === "panel_db"
              ? t("Encryption passphrase (required, 12+ characters)")
              : t("Encryption passphrase (optional)")
          }
        >
          <TextInput
            type="password"
            autoComplete="new-password"
            required={form.target_kind === "panel_db"}
            minLength={form.target_kind === "panel_db" ? 12 : undefined}
            value={form.passphrase}
            onChange={set("passphrase")}
          />
        </Field>
        {form.target_kind === "panel_db" ? (
          <label className="flex items-center gap-2 self-end text-sm">
            <input
              type="checkbox"
              checked={form.include_history}
              onChange={check("include_history")}
            />
            {t("Include log lines, metric samples, and sessions")}
          </label>
        ) : null}
      </div>
      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">{t("Destinations")}</legend>
        <div className="flex flex-wrap gap-3">
          {(destinations.data ?? []).map((destination) => (
            <label key={destination.id} className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={chosen.includes(destination.id)}
                onChange={(event) =>
                  setChosen(
                    event.target.checked
                      ? [...chosen, destination.id]
                      : chosen.filter((id) => id !== destination.id),
                  )
                }
              />
              {destination.name}
            </label>
          ))}
        </div>
      </fieldset>
      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">{t("Retention")}</legend>
        <p className="text-[13px] text-muted">
          {t("Old backups are deleted once no rule below wants them. Empty fields are not used.")}
        </p>
        <QuickPick
          label={t("Retention presets")}
          options={RETENTION.map((preset) => ({
            label: preset.label,
            value: retentionKey(preset.values),
            hint: preset.hint,
          }))}
          value={retentionKey(form)}
          onPick={(_, option) => {
            const preset = RETENTION.find((item) => item.label === option.label);
            if (preset) assign(preset.values);
          }}
        />
        <div className="grid gap-3 md:grid-cols-5">
          <Field label={t("Keep last")}>
            <TextInput value={form.keep_last} onChange={set("keep_last")} />
          </Field>
          <Field label={t("Keep days")}>
            <TextInput value={form.keep_days} onChange={set("keep_days")} />
          </Field>
          <Field label={t("Daily")}>
            <TextInput value={form.daily} onChange={set("daily")} />
          </Field>
          <Field label={t("Weekly")}>
            <TextInput value={form.weekly} onChange={set("weekly")} />
          </Field>
          <Field label={t("Monthly")}>
            <TextInput value={form.monthly} onChange={set("monthly")} />
          </Field>
        </div>
      </fieldset>
      <Disclosure title={t("Advanced options")}>
        <div className="grid gap-3 md:grid-cols-2">
          <Field label={t("Path prefix")}>
            <TextInput value={form.path_prefix} onChange={set("path_prefix")} />
          </Field>
          <Field
            label={t("Filename template ({tokens})", {
              tokens: (presets.data?.filename_tokens ?? []).join(" "),
            })}
            presets={
              <QuickPick
                label={t("Filename tokens")}
                options={(presets.data?.filename_tokens ?? []).map((token) => ({
                  label: `+ ${token}`,
                  value: token,
                }))}
                onPick={(token) => assign({ filename_template: form.filename_template + token })}
              />
            }
          >
            <TextInput value={form.filename_template} onChange={set("filename_template")} />
          </Field>
          {isPanel ? null : isArchive ? (
            <>
              <Field label={t("Include globs (one per line)")}>
                <TextArea value={form.include_globs} onChange={set("include_globs")} />
              </Field>
              <Field
                label={t("Exclude globs (one per line)")}
                presets={
                  <QuickPick
                    label={t("Common exclusions")}
                    options={EXCLUDE_GLOBS}
                    onPick={(glob) => appendLine("exclude_globs", glob)}
                  />
                }
              >
                <TextArea value={form.exclude_globs} onChange={set("exclude_globs")} />
              </Field>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={form.stop_service}
                  onChange={check("stop_service")}
                />
                {t("Stop the service during the backup")}
              </label>
            </>
          ) : (
            <>
              <label className="flex items-center gap-2 text-sm">
                <input type="checkbox" checked={form.dump_all} onChange={check("dump_all")} />
                {t("Postgres: pg_dumpall (all databases and roles)")}
              </label>
              <Field label={t("Extra dump arguments")}>
                <TextInput value={form.extra_args} onChange={set("extra_args")} />
              </Field>
              <Field label={t("Include databases")}>
                <TextInput value={form.include_databases} onChange={set("include_databases")} />
              </Field>
              <Field label={t("Exclude databases")}>
                <TextInput value={form.exclude_databases} onChange={set("exclude_databases")} />
              </Field>
              <Field label={t("Include tables")}>
                <TextInput value={form.include_tables} onChange={set("include_tables")} />
              </Field>
              <Field label={t("Exclude tables")}>
                <TextInput value={form.exclude_tables} onChange={set("exclude_tables")} />
              </Field>
            </>
          )}
          {isPanel ? null : (
            <>
              <Field label={t("Pre-hook command (runs in the container)")}>
                <TextInput
                  value={form.pre_hook}
                  placeholder="sh -c 'echo start'"
                  onChange={set("pre_hook")}
                />
              </Field>
              <Field label={t("Post-hook command")}>
                <TextInput value={form.post_hook} onChange={set("post_hook")} />
              </Field>
            </>
          )}
          <Field
            label={t("Timeout (seconds)")}
            presets={
              <QuickPick
                label={t("Timeout presets")}
                options={pickDurations("900", "1800", "3600", "21600", "86400")}
                value={form.timeout_secs}
                onPick={(timeout_secs) => assign({ timeout_secs })}
              />
            }
          >
            <TextInput value={form.timeout_secs} onChange={set("timeout_secs")} />
          </Field>
          <Field
            label={t("Retries")}
            presets={
              <QuickPick
                label={t("Retry presets")}
                options={RETRIES}
                value={form.retries}
                onPick={(retries) => assign({ retries })}
              />
            }
          >
            <TextInput value={form.retries} onChange={set("retries")} />
          </Field>
          <Field
            label={t("Bandwidth limit (Mbit/s, empty = unlimited)")}
            presets={
              <QuickPick
                label={t("Bandwidth presets")}
                options={BANDWIDTH}
                value={form.bandwidth_mbps}
                onPick={(bandwidth_mbps) => assign({ bandwidth_mbps })}
              />
            }
          >
            <TextInput value={form.bandwidth_mbps} onChange={set("bandwidth_mbps")} />
          </Field>
          <div className="space-y-2 text-sm">
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={form.verify_after_upload}
                onChange={check("verify_after_upload")}
              />
              {t("Verify checksum after upload")}
            </label>
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={form.notify_on_success}
                onChange={check("notify_on_success")}
              />
              {t("Notify on success too")}
            </label>
          </div>
        </div>
      </Disclosure>
      <Button type="submit" disabled={save.isPending}>
        {t("Save backup job")}
      </Button>
    </form>
  );
}

export function BackupJobs({ serviceId }: { serviceId?: string }) {
  const t = useT();
  const queryClient = useQueryClient();
  const jobs = useQuery({
    queryKey: ["backup-jobs", serviceId ?? "all"],
    queryFn: () =>
      api<Job[]>(serviceId ? `/api/v1/backup-jobs?service_id=${serviceId}` : "/api/v1/backup-jobs"),
    refetchInterval: 5000,
  });
  const [open, setOpen] = useState<string | null>(null);

  const run = useMutation({
    mutationFn: (id: string) =>
      api<Schemas["RunAccepted"]>(`/api/v1/backup-jobs/${id}/run`, { method: "POST" }),
    onSuccess: async (_, id) => {
      toast.success(t("Backup started"));
      setOpen(id);
      await queryClient.invalidateQueries({ queryKey: ["backup-runs", id] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not start the backup"))),
  });
  const toggle = useMutation({
    mutationFn: (job: Job) =>
      api(`/api/v1/backup-jobs/${job.id}`, {
        method: "PATCH",
        body: JSON.stringify({ enabled: !job.enabled }),
      }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["backup-jobs"] }),
    onError: (error) => toast.error(errorMessage(error, t("Could not update the job"))),
  });
  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/backup-jobs/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success(t("Backup job deleted"));
      await queryClient.invalidateQueries({ queryKey: ["backup-jobs"] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not delete the job"))),
  });

  if (jobs.data?.length === 0) {
    return (
      <Empty icon="archive" title={t("No backup jobs yet")}>
        {t("Create a job to back up on a schedule and keep copies in S3.")}
      </Empty>
    );
  }
  return (
    <ul className="space-y-3">
      {(jobs.data ?? []).map((job) => (
        <li key={job.id} className="overflow-hidden rounded-alert border border-line bg-surface">
          <div className="flex flex-wrap items-center gap-x-4 gap-y-3 p-4">
            <div className="min-w-0 flex-1 basis-72">
              <p className="flex flex-wrap items-center gap-2 font-medium text-ink">
                {job.name}
                {job.last_status ? <Badge value={job.last_status} /> : null}
                {job.enabled ? null : <Badge value="paused" tone="outline" />}
              </p>
              <p className="mt-1 text-[13px] text-muted">
                {job.target_kind} · {job.schedule_cron || t("manual")} ({job.timezone}) ·{" "}
                {t("next {time}", { time: formatTime(job.next_run_at) })} · {job.compression}
                {job.encrypted ? ` · ${t("encrypted")}` : ""} ·{" "}
                {t("{count} destination(s)", { count: job.destination_ids.length })}
              </p>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <Button
                size="sm"
                icon="play"
                onClick={() => run.mutate(job.id)}
                disabled={run.isPending}
              >
                {t("Back up now")}
              </Button>
              <Button
                size="sm"
                variant="secondary"
                icon={open === job.id ? "chevronDown" : "chevronRight"}
                aria-expanded={open === job.id}
                onClick={() => setOpen(open === job.id ? null : job.id)}
              >
                {open === job.id ? t("Hide runs") : t("Runs")}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => toggle.mutate(job)}>
                {job.enabled ? t("Pause") : t("Resume")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                icon="trash"
                className="text-danger hover:bg-danger-soft hover:text-danger"
                aria-label={t("Delete backup job {name}", { name: job.name })}
                title={t("Delete")}
                onClick={() => {
                  if (
                    window.confirm(
                      t("Delete backup job {name}? Stored objects are kept.", { name: job.name }),
                    )
                  ) {
                    remove.mutate(job.id);
                  }
                }}
              />
            </div>
          </div>
          {open === job.id ? <RunHistory job={job} /> : null}
        </li>
      ))}
    </ul>
  );
}

function RunHistory({ job }: { job: Job }) {
  const t = useT();
  const runs = useQuery({
    queryKey: ["backup-runs", job.id],
    queryFn: () => api<Run[]>(`/api/v1/backup-jobs/${job.id}/runs`),
    refetchInterval: (query) =>
      (query.state.data ?? []).some((run) => run.status === "queued" || run.status === "running")
        ? 1500
        : 10000,
  });
  const [logsFor, setLogsFor] = useState<string | null>(null);
  const [restoreFor, setRestoreFor] = useState<string | null>(null);

  if (runs.data?.length === 0) {
    return (
      <div className="border-t border-line">
        <Empty>{t("No runs yet.")}</Empty>
      </div>
    );
  }
  return (
    <DataTable
      minWidth={640}
      head={
        <>
          <Th>{t("Started")}</Th>
          <Th>{t("Kind")}</Th>
          <Th>{t("Status")}</Th>
          <Th>{t("Duration")}</Th>
          <Th>{t("Size")}</Th>
          <Th>
            <span className="sr-only">{t("Actions")}</span>
          </Th>
        </>
      }
    >
      {(runs.data ?? []).map((run) => (
        <RunRow
          key={run.id}
          job={job}
          run={run}
          showLogs={logsFor === run.id}
          showRestore={restoreFor === run.id}
          onLogs={() => setLogsFor(logsFor === run.id ? null : run.id)}
          onRestore={() => setRestoreFor(restoreFor === run.id ? null : run.id)}
        />
      ))}
    </DataTable>
  );
}

function RunRow({
  job,
  run,
  showLogs,
  showRestore,
  onLogs,
  onRestore,
}: {
  job: Job;
  run: Run;
  showLogs: boolean;
  showRestore: boolean;
  onLogs: () => void;
  onRestore: () => void;
}) {
  const t = useT();
  const restorable =
    run.kind === "backup" && (run.status === "succeeded" || run.status === "partial");
  return (
    <>
      <tr>
        <Td className="whitespace-nowrap">{formatTime(run.started_at)}</Td>
        <Td className="whitespace-nowrap text-muted">
          {run.kind} · {run.trigger}
        </Td>
        <Td>
          <Badge value={run.status} />
        </Td>
        <Td className="tabular-nums">{formatDuration(run.duration_ms)}</Td>
        <Td className="whitespace-nowrap tabular-nums">{formatBytes(run.size_bytes)}</Td>
        <Td className="text-right whitespace-nowrap">
          <Button size="sm" variant="ghost" onClick={onLogs} aria-expanded={showLogs}>
            {t("Logs")}
          </Button>
          {restorable ? (
            <>
              <a
                className={buttonClass({ variant: "ghost", size: "sm" })}
                href={`/api/v1/backup-runs/${run.id}/download`}
              >
                <Icon name="download" size={14} />
                {t("Download")}
              </a>
              <Button size="sm" variant="ghost" icon="refresh" onClick={onRestore}>
                {t("Restore")}
              </Button>
            </>
          ) : null}
        </Td>
      </tr>
      {showLogs ? (
        <tr>
          <Td colSpan={6} className="bg-paper/60">
            {run.error ? <p className="mb-2 text-sm text-danger">{run.error}</p> : null}
            {run.checksum ? (
              <p className="font-mono text-xs break-all text-muted">sha256 {run.checksum}</p>
            ) : null}
            {run.object_keys.map((key) => (
              <p key={key} className="font-mono text-xs break-all text-muted">
                {key}
              </p>
            ))}
            <div className="mt-2">
              <LogPre className="max-h-64" empty={t("No log output yet.")}>
                {run.logs}
              </LogPre>
            </div>
          </Td>
        </tr>
      ) : null}
      {showRestore ? (
        <tr>
          <Td colSpan={6} className="bg-paper/60">
            {job.target_kind.startsWith("panel_") ? (
              <PanelRestoreHint />
            ) : (
              <RestoreForm job={job} run={run} />
            )}
          </Td>
        </tr>
      ) : null}
    </>
  );
}

export function PanelRestoreHint() {
  const t = useT();
  return (
    <div className="my-1 space-y-2 rounded-alert border border-line bg-surface p-4 text-sm">
      <p>
        {t(
          "Panel backups are restored on a fresh host before the panel starts. Download the backup, copy it to the new server, and run:",
        )}
      </p>
      <pre className="overflow-auto rounded-alert border border-line bg-paper p-3 font-mono text-xs">
        curl -fsSL https://github.com/vantanminh/lunchdock/releases/latest/download/install.sh -o
        install.sh
        {"\n"}sudo sh install.sh --restore rudder-panel-….tar.zst.enc
      </pre>
      <p className="text-muted">
        {t(
          "The installer asks for the backup passphrase and writes the master key from the backup.",
        )}
      </p>
    </div>
  );
}

function RestoreForm({ job, run }: { job: Job; run: Run }) {
  const t = useT();
  const queryClient = useQueryClient();
  const [targetService, setTargetService] = useState(job.service_id ?? "");
  const [target, setTarget] = useState("");
  const restore = useMutation({
    mutationFn: () =>
      api<Schemas["RunAccepted"]>(`/api/v1/backup-runs/${run.id}/restore`, {
        method: "POST",
        body: JSON.stringify({
          target_service_id:
            job.target_kind === "database" ? targetService || undefined : undefined,
          target_volume: job.target_kind === "volume" ? target || undefined : undefined,
          target_path:
            job.target_kind !== "volume" && job.target_kind !== "database"
              ? target || undefined
              : undefined,
        }),
      }),
    onSuccess: async () => {
      toast.success(t("Restore started; it appears in the run list"));
      await queryClient.invalidateQueries({ queryKey: ["backup-runs", job.id] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not start the restore"))),
  });
  return (
    <form
      className="my-1 flex flex-wrap items-end gap-3 rounded-alert border border-line bg-surface p-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (window.confirm(t("Restoring overwrites the target data. Continue?"))) {
          restore.mutate();
        }
      }}
    >
      {job.target_kind === "database" ? (
        <Field label={t("Target database service ID (default: same service)")}>
          <TextInput
            value={targetService}
            onChange={(event) => setTargetService(event.target.value)}
          />
        </Field>
      ) : (
        <Field
          label={
            job.target_kind === "volume"
              ? t("Target volume (empty = original)")
              : t("Target host path (empty = original)")
          }
        >
          <TextInput value={target} onChange={(event) => setTarget(event.target.value)} />
        </Field>
      )}
      <Button type="submit" disabled={restore.isPending}>
        {t("Restore")}
      </Button>
    </form>
  );
}
