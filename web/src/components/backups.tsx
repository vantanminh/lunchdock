import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { api } from "../api";
import { errorMessage, formatBytes, formatDuration, formatTime } from "../format";
import type { Schemas } from "../types";
import { Badge, Button, Empty, Field, Select, TextArea, TextInput } from "./ui";

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

export function BackupJobForm({ target, onSaved }: { target: BackupTarget; onSaved?: () => void }) {
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
  const [advanced, setAdvanced] = useState(false);

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
      toast.success("Backup job saved");
      setForm({ ...form, name: "", passphrase: "" });
      await queryClient.invalidateQueries({ queryKey: ["backup-jobs"] });
      onSaved?.();
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save the backup job")),
  });

  const set = (key: keyof typeof form) => (event: { target: { value: string } }) =>
    setForm({ ...form, [key]: event.target.value });
  const check = (key: keyof typeof form) => (event: { target: { checked: boolean } }) =>
    setForm({ ...form, [key]: event.target.checked });
  const isPanel = form.target_kind.startsWith("panel_");
  const isArchive = form.target_kind !== "database" && !isPanel;
  const presetNames = (presets.data?.schedules ?? []).map((preset) => preset.name);

  if (destinations.data?.length === 0) {
    return (
      <Empty>
        Add an{" "}
        <Link className="text-accent" to="/s3-destinations">
          S3 destination
        </Link>{" "}
        before creating backup jobs.
      </Empty>
    );
  }

  return (
    <form
      className="space-y-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (chosen.length === 0) {
          toast.error("Choose at least one destination");
          return;
        }
        save.mutate();
      }}
    >
      <div className="grid gap-3 md:grid-cols-2">
        <Field label="Job name">
          <TextInput required value={form.name} placeholder="nightly" onChange={set("name")} />
        </Field>
        <Field label="What to back up">
          <Select value={form.target_kind} onChange={set("target_kind")}>
            {target.panel ? (
              <>
                <option value="panel_db">Panel database and master key</option>
                <option value="panel_settings">Settings export (JSON, no secrets)</option>
              </>
            ) : (
              <>
                {target.isDatabase ? <option value="database">Database dump</option> : null}
                <option value="volume">Docker volume</option>
                <option value="bind">Bind mount</option>
                <option value="path">Host path</option>
              </>
            )}
          </Select>
        </Field>
        {!target.serverId && !target.serviceId && !target.panel ? (
          <Field label="Server">
            <Select required value={form.server_id} onChange={set("server_id")}>
              <option value="">Choose</option>
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
                ? "Volume name"
                : form.target_kind === "bind"
                  ? "Bind source on the host"
                  : "Absolute host path"
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
        <Field label="Schedule (cron or preset, empty = manual only)">
          <TextInput
            list="backup-presets"
            value={form.schedule_cron}
            onChange={set("schedule_cron")}
          />
          <datalist id="backup-presets">
            {presetNames.map((name) => (
              <option key={name} value={name} />
            ))}
          </datalist>
        </Field>
        <Field label="Timezone">
          <TextInput required value={form.timezone} onChange={set("timezone")} />
        </Field>
        <Field label="Compression">
          <div className="flex gap-2">
            <Select value={form.compression} onChange={set("compression")}>
              <option value="zstd">zstd</option>
              <option value="gzip">gzip</option>
              <option value="none">none</option>
            </Select>
            <TextInput
              aria-label="Compression level"
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
              ? "Encryption passphrase (required, 12+ characters)"
              : "Encryption passphrase (optional)"
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
            Include log lines, metric samples, and sessions
          </label>
        ) : null}
      </div>
      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">Destinations</legend>
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
      <div className="grid gap-3 md:grid-cols-5">
        <Field label="Keep last">
          <TextInput value={form.keep_last} onChange={set("keep_last")} />
        </Field>
        <Field label="Keep days">
          <TextInput value={form.keep_days} onChange={set("keep_days")} />
        </Field>
        <Field label="Daily">
          <TextInput value={form.daily} onChange={set("daily")} />
        </Field>
        <Field label="Weekly">
          <TextInput value={form.weekly} onChange={set("weekly")} />
        </Field>
        <Field label="Monthly">
          <TextInput value={form.monthly} onChange={set("monthly")} />
        </Field>
      </div>
      <button type="button" className="text-sm text-accent" onClick={() => setAdvanced(!advanced)}>
        {advanced ? "Hide advanced options" : "Show advanced options"}
      </button>
      {advanced ? (
        <div className="grid gap-3 md:grid-cols-2">
          <Field label="Path prefix">
            <TextInput value={form.path_prefix} onChange={set("path_prefix")} />
          </Field>
          <Field label={`Filename template (${(presets.data?.filename_tokens ?? []).join(" ")})`}>
            <TextInput value={form.filename_template} onChange={set("filename_template")} />
          </Field>
          {isPanel ? null : isArchive ? (
            <>
              <Field label="Include globs (one per line)">
                <TextArea value={form.include_globs} onChange={set("include_globs")} />
              </Field>
              <Field label="Exclude globs (one per line)">
                <TextArea value={form.exclude_globs} onChange={set("exclude_globs")} />
              </Field>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={form.stop_service}
                  onChange={check("stop_service")}
                />
                Stop the service during the backup
              </label>
            </>
          ) : (
            <>
              <label className="flex items-center gap-2 text-sm">
                <input type="checkbox" checked={form.dump_all} onChange={check("dump_all")} />
                Postgres: pg_dumpall (all databases and roles)
              </label>
              <Field label="Extra dump arguments">
                <TextInput value={form.extra_args} onChange={set("extra_args")} />
              </Field>
              <Field label="Include databases">
                <TextInput value={form.include_databases} onChange={set("include_databases")} />
              </Field>
              <Field label="Exclude databases">
                <TextInput value={form.exclude_databases} onChange={set("exclude_databases")} />
              </Field>
              <Field label="Include tables">
                <TextInput value={form.include_tables} onChange={set("include_tables")} />
              </Field>
              <Field label="Exclude tables">
                <TextInput value={form.exclude_tables} onChange={set("exclude_tables")} />
              </Field>
            </>
          )}
          {isPanel ? null : (
            <>
              <Field label="Pre-hook command (runs in the container)">
                <TextInput
                  value={form.pre_hook}
                  placeholder="sh -c 'echo start'"
                  onChange={set("pre_hook")}
                />
              </Field>
              <Field label="Post-hook command">
                <TextInput value={form.post_hook} onChange={set("post_hook")} />
              </Field>
            </>
          )}
          <Field label="Timeout (seconds)">
            <TextInput value={form.timeout_secs} onChange={set("timeout_secs")} />
          </Field>
          <Field label="Retries">
            <TextInput value={form.retries} onChange={set("retries")} />
          </Field>
          <Field label="Bandwidth limit (Mbit/s, empty = unlimited)">
            <TextInput value={form.bandwidth_mbps} onChange={set("bandwidth_mbps")} />
          </Field>
          <div className="space-y-2 text-sm">
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={form.verify_after_upload}
                onChange={check("verify_after_upload")}
              />
              Verify checksum after upload
            </label>
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={form.notify_on_success}
                onChange={check("notify_on_success")}
              />
              Notify on success too
            </label>
          </div>
        </div>
      ) : null}
      <Button type="submit" disabled={save.isPending}>
        Save backup job
      </Button>
    </form>
  );
}

export function BackupJobs({ serviceId }: { serviceId?: string }) {
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
      toast.success("Backup started");
      setOpen(id);
      await queryClient.invalidateQueries({ queryKey: ["backup-runs", id] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not start the backup")),
  });
  const toggle = useMutation({
    mutationFn: (job: Job) =>
      api(`/api/v1/backup-jobs/${job.id}`, {
        method: "PATCH",
        body: JSON.stringify({ enabled: !job.enabled }),
      }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["backup-jobs"] }),
    onError: (error) => toast.error(errorMessage(error, "Could not update the job")),
  });
  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/backup-jobs/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success("Backup job deleted");
      await queryClient.invalidateQueries({ queryKey: ["backup-jobs"] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not delete the job")),
  });

  if (jobs.data?.length === 0) {
    return <Empty>No backup jobs yet.</Empty>;
  }
  return (
    <ul className="space-y-3">
      {(jobs.data ?? []).map((job) => (
        <li key={job.id} className="rounded-lg border border-line p-3">
          <div className="flex flex-wrap items-center gap-2">
            <div className="min-w-0 flex-1">
              <p className="font-medium">
                {job.name} {job.last_status ? <Badge value={job.last_status} /> : null}
                {job.enabled ? null : <span className="ml-2 text-xs text-muted">paused</span>}
              </p>
              <p className="text-sm text-muted">
                {job.target_kind} · {job.schedule_cron || "manual"} ({job.timezone}) · next{" "}
                {formatTime(job.next_run_at)} · {job.compression}
                {job.encrypted ? " · encrypted" : ""} · {job.destination_ids.length} destination(s)
              </p>
            </div>
            <Button onClick={() => run.mutate(job.id)} disabled={run.isPending}>
              Back up now
            </Button>
            <Button quiet onClick={() => setOpen(open === job.id ? null : job.id)}>
              {open === job.id ? "Hide runs" : "Runs"}
            </Button>
            <Button quiet onClick={() => toggle.mutate(job)}>
              {job.enabled ? "Pause" : "Resume"}
            </Button>
            <Button
              quiet
              onClick={() => {
                if (window.confirm(`Delete backup job ${job.name}? Stored objects are kept.`)) {
                  remove.mutate(job.id);
                }
              }}
            >
              Delete
            </Button>
          </div>
          {open === job.id ? <RunHistory job={job} /> : null}
        </li>
      ))}
    </ul>
  );
}

function RunHistory({ job }: { job: Job }) {
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

  return (
    <div className="mt-3 overflow-x-auto">
      <table className="w-full min-w-[640px] text-left text-sm">
        <thead className="text-xs text-muted uppercase">
          <tr>
            <th className="py-1">Started</th>
            <th>Kind</th>
            <th>Status</th>
            <th>Duration</th>
            <th>Size</th>
            <th />
          </tr>
        </thead>
        <tbody className="divide-y divide-line">
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
        </tbody>
      </table>
      {runs.data?.length === 0 ? <Empty>No runs yet.</Empty> : null}
    </div>
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
  const restorable =
    run.kind === "backup" && (run.status === "succeeded" || run.status === "partial");
  return (
    <>
      <tr>
        <td className="py-2">{formatTime(run.started_at)}</td>
        <td>
          {run.kind} · {run.trigger}
        </td>
        <td>
          <Badge value={run.status} />
        </td>
        <td>{formatDuration(run.duration_ms)}</td>
        <td>{formatBytes(run.size_bytes)}</td>
        <td className="space-x-2 text-right whitespace-nowrap">
          <button type="button" className="text-accent" onClick={onLogs}>
            Logs
          </button>
          {restorable ? (
            <>
              <a className="text-accent" href={`/api/v1/backup-runs/${run.id}/download`}>
                Download
              </a>
              <button type="button" className="text-accent" onClick={onRestore}>
                Restore
              </button>
            </>
          ) : null}
        </td>
      </tr>
      {showLogs ? (
        <tr>
          <td colSpan={6}>
            {run.error ? <p className="text-sm text-red-600">{run.error}</p> : null}
            {run.checksum ? (
              <p className="font-mono text-xs break-all text-muted">sha256 {run.checksum}</p>
            ) : null}
            {run.object_keys.map((key) => (
              <p key={key} className="font-mono text-xs text-muted">
                {key}
              </p>
            ))}
            <pre className="mt-1 max-h-64 overflow-auto rounded-md bg-paper p-2 text-xs whitespace-pre-wrap">
              {run.logs || "No log output yet."}
            </pre>
          </td>
        </tr>
      ) : null}
      {showRestore ? (
        <tr>
          <td colSpan={6}>
            {job.target_kind.startsWith("panel_") ? (
              <PanelRestoreHint />
            ) : (
              <RestoreForm job={job} run={run} />
            )}
          </td>
        </tr>
      ) : null}
    </>
  );
}

export function PanelRestoreHint() {
  return (
    <div className="my-2 space-y-2 rounded-md border border-line p-3 text-sm">
      <p>
        Panel backups are restored on a fresh host before the panel starts. Download the backup,
        copy it to the new server, and run:
      </p>
      <pre className="overflow-auto rounded-md bg-paper p-2 font-mono text-xs">
        curl -fsSL https://github.com/vantanminh/lunchdock/releases/latest/download/install.sh -o
        install.sh
        {"\n"}sudo sh install.sh --restore rudder-panel-….tar.zst.enc
      </pre>
      <p className="text-muted">
        The installer asks for the backup passphrase and writes the master key from the backup.
      </p>
    </div>
  );
}

function RestoreForm({ job, run }: { job: Job; run: Run }) {
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
      toast.success("Restore started; it appears in the run list");
      await queryClient.invalidateQueries({ queryKey: ["backup-runs", job.id] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not start the restore")),
  });
  return (
    <form
      className="my-2 flex flex-wrap items-end gap-3 rounded-md border border-line p-3"
      onSubmit={(event) => {
        event.preventDefault();
        if (window.confirm("Restoring overwrites the target data. Continue?")) {
          restore.mutate();
        }
      }}
    >
      {job.target_kind === "database" ? (
        <Field label="Target database service ID (default: same service)">
          <TextInput
            value={targetService}
            onChange={(event) => setTargetService(event.target.value)}
          />
        </Field>
      ) : (
        <Field
          label={
            job.target_kind === "volume"
              ? "Target volume (empty = original)"
              : "Target host path (empty = original)"
          }
        >
          <TextInput value={target} onChange={(event) => setTarget(event.target.value)} />
        </Field>
      )}
      <Button type="submit" disabled={restore.isPending}>
        Restore
      </Button>
    </form>
  );
}
