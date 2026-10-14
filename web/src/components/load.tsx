import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState, type ReactNode } from "react";
import { Link, useLocation } from "react-router-dom";
import { toast } from "sonner";
import { api } from "../api";
import { useAuth } from "../auth";
import { errorMessage, formatBytes, formatRelative } from "../format";
import { msg, useT, type Translate } from "../i18n";
import type { Schemas } from "../types";
import { Icon, type IconName } from "./icons";
import { Badge, Button, buttonClass, Dialog, Empty, KindTile, Segmented, Skeleton } from "./ui";

export type LoadView = Schemas["LoadView"];
type Guard = Schemas["GuardView"];
type Pressure = Schemas["HostPressure"];
type Reason = Schemas["GuardReason"];
type Level = Schemas["GuardLevel"];
type Service = Schemas["ServiceSummary"];
type Server = Schemas["ServerView"];
type Usage = Schemas["ServiceUsage"];

/** The load guard samples every 3 seconds; the panel asks a little less often. */
export const LOAD_REFRESH = 5_000;

export function useLoad() {
  return useQuery({
    queryKey: ["load"],
    queryFn: () => api<LoadView>("/api/v1/load"),
    refetchInterval: LOAD_REFRESH,
  });
}

const levelTone: Record<Level, string> = { ok: "success", busy: "warning", critical: "danger" };
const levelLabel: Record<Level, string> = {
  ok: msg("Normal"),
  busy: msg("Busy"),
  critical: msg("Critical"),
};

export function LevelBadge({ level }: { level: Level }) {
  return <Badge value={levelLabel[level]} tone={levelTone[level]} />;
}

const round = (value: number) => Math.round(value);
/** A threshold such as 7.5 keeps its decimal; whole numbers lose it. */
const threshold = (value: number) => (Number.isInteger(value) ? String(value) : value.toFixed(1));

/** One reason the guard is not at normal, as a sentence. */
export function reasonText(t: Translate, reason: Reason): string {
  const values = { value: round(reason.value), limit: threshold(reason.limit) };
  switch (reason.code) {
    case "memory_low":
      return t("Free memory {value}% is below the {limit}% reserve", values);
    case "memory_critical":
      return t("Free memory {value}% is below {limit}%", values);
    case "memory_stall":
      return t("Processes waited for memory {value}% of the time", values);
    case "cpu_high":
      return t("CPU at {value}% for the last 15 seconds (limit {limit}%)", values);
    case "settling":
      return t("Back under the limits; waiting for the server to settle");
    default:
      return reason.code;
  }
}

/** Why queued builds wait, as a sentence, or null when they may start. */
export function holdText(t: Translate, hold: string | null | undefined): string | null {
  switch (hold) {
    case "paused":
      return t("Builds are paused");
    case "busy":
      return t("New builds wait until the server has room");
    case "critical":
      return t("Builds are stopped until memory is free again");
    default:
      return null;
  }
}

/** A usage bar with the guard's thresholds marked on it. */
export function ThresholdMeter({
  value,
  warn,
  danger,
  label,
  marks = true,
}: {
  value: number | null | undefined;
  warn?: number;
  danger?: number;
  label: string;
  /** Draw the thresholds as ticks; off where the bar only colors by them. */
  marks?: boolean;
}) {
  const pct = value == null ? null : Math.max(0, Math.min(100, value));
  const fill =
    pct == null
      ? ""
      : danger != null && pct >= danger
        ? "bg-danger"
        : warn != null && pct >= warn
          ? "bg-amber"
          : "bg-pine";
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={pct ?? undefined}
      className="relative h-2 rounded-full bg-sunken"
    >
      {pct != null ? (
        <div
          className={`h-full rounded-full transition-[width] duration-500 ${fill}`}
          style={{ width: `${Math.max(pct, 1.5)}%` }}
        />
      ) : null}
      {[warn, danger].map((mark, index) =>
        marks && pct != null && mark != null && mark < 100 ? (
          <span
            key={index}
            aria-hidden="true"
            className={`absolute -top-0.5 -bottom-0.5 w-0.5 rounded-full ${index === 0 ? "bg-amber/70" : "bg-danger/70"}`}
            style={{ left: `calc(${mark}% - 1px)` }}
          />
        ) : null,
      )}
    </div>
  );
}

function Gauge({
  icon,
  label,
  value,
  detail,
  warn,
  danger,
}: {
  icon: IconName;
  label: string;
  value: number | null;
  detail: ReactNode;
  warn?: number;
  danger?: number;
}) {
  const t = useT();
  return (
    <div className="min-w-0">
      <div className="flex items-baseline justify-between gap-2">
        <span className="flex items-center gap-1.5 text-[13px] font-medium text-ink-soft">
          <Icon name={icon} size={14} className="text-faint" />
          {label}
        </span>
        <span className="text-lg leading-none font-semibold text-ink tabular-nums">
          {value == null ? "—" : `${round(value)}%`}
        </span>
      </div>
      <div className="mt-2.5">
        <ThresholdMeter
          value={value}
          warn={warn}
          danger={danger}
          label={t("{label} usage", { label })}
        />
      </div>
      <p className="mt-2 truncate text-xs text-muted tabular-nums">{detail}</p>
    </div>
  );
}

function memoryUsed(pressure: Pressure) {
  return pressure.memory_total_bytes > 0
    ? 100 - (pressure.memory_available_bytes / pressure.memory_total_bytes) * 100
    : null;
}

/** Memory, CPU, swap and pressure of the panel server with the guard's lines drawn in. */
export function PressureGauges({ guard, compact = false }: { guard: Guard; compact?: boolean }) {
  const t = useT();
  const pressure = guard.pressure;
  if (!pressure) {
    return (
      <p className="text-sm text-muted">
        {guard.error
          ? t("Could not read the server's load: {error}", { error: guard.error })
          : t("Waiting for the first reading…")}
      </p>
    );
  }
  const used = memoryUsed(pressure);
  const swapUsed =
    pressure.swap_total_bytes > 0
      ? 100 - (pressure.swap_free_bytes / pressure.swap_total_bytes) * 100
      : null;
  const settings = guard.settings;
  return (
    <div className={`grid gap-x-6 gap-y-5 ${compact ? "sm:grid-cols-2" : "sm:grid-cols-2 xl:grid-cols-4"}`}>
      <Gauge
        icon="memory"
        label={t("Memory")}
        value={used}
        warn={100 - settings.memory_reserve_percent}
        danger={100 - guard.critical_memory_percent}
        detail={t("{free} free of {total}", {
          free: formatBytes(pressure.memory_available_bytes),
          total: formatBytes(pressure.memory_total_bytes),
        })}
      />
      <Gauge
        icon="cpu"
        label={t("CPU")}
        value={pressure.cpu_percent}
        warn={settings.cpu_percent}
        detail={t("{cores} cores · load {load}", {
          cores: pressure.cpu_count,
          load: pressure.load1.toFixed(2),
        })}
      />
      {compact ? null : (
        <>
          <Gauge
            icon="archive"
            label={t("Swap")}
            value={swapUsed}
            warn={50}
            danger={80}
            detail={
              pressure.swap_total_bytes > 0
                ? t("{used} used of {total}", {
                    used: formatBytes(pressure.swap_total_bytes - pressure.swap_free_bytes),
                    total: formatBytes(pressure.swap_total_bytes),
                  })
                : t("No swap on this server")
            }
          />
          <Gauge
            icon="activity"
            label={t("Waiting for memory")}
            value={pressure.memory_pressure ?? null}
            warn={10}
            danger={25}
            detail={
              pressure.memory_pressure == null
                ? t("The kernel does not report pressure")
                : t("Stalled {full}% · I/O {io}% · CPU {cpu}%", {
                    full: round(pressure.memory_stall ?? 0),
                    io: round(pressure.io_pressure ?? 0),
                    cpu: round(pressure.cpu_pressure ?? 0),
                  })
            }
          />
        </>
      )}
    </div>
  );
}

export function useLoadActions() {
  const t = useT();
  const queryClient = useQueryClient();
  const refresh = (view?: LoadView) => {
    if (view) {
      queryClient.setQueryData(["load"], view);
    }
    void queryClient.invalidateQueries({ queryKey: ["load"] });
    void queryClient.invalidateQueries({ queryKey: ["events"] });
  };
  const pause = useMutation({
    mutationFn: (paused: boolean) =>
      api<LoadView>("/api/v1/load/pause", {
        method: "POST",
        body: JSON.stringify({ paused }),
      }),
    onSuccess: (view, paused) => {
      refresh(view);
      toast.success(paused ? t("Builds paused") : t("Builds resumed"));
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not change the build queue"))),
  });
  const cancel = useMutation({
    mutationFn: () =>
      api<Schemas["BuildsCancelled"]>("/api/v1/load/cancel-builds", { method: "POST" }),
    onSuccess: (result) => {
      refresh();
      void queryClient.invalidateQueries({ queryKey: ["services"] });
      toast.success(
        t("Cancelled {queued} queued and stopped {running} running builds", {
          queued: result.queued,
          running: result.running,
        }),
      );
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not cancel the builds"))),
  });
  return { pause, cancel };
}

/** Pause or resume builds and cancel them all; admins only. */
export function BuildActions({ view, size = "md" }: { view: LoadView; size?: "sm" | "md" }) {
  const t = useT();
  const { user } = useAuth();
  const { pause, cancel } = useLoadActions();
  const [confirm, setConfirm] = useState(false);
  if (user?.role !== "admin") {
    return null;
  }
  const paused = view.guard.settings.builds_paused;
  const active = view.builds.queued + view.builds.running + view.builds.stack_builds;
  return (
    <>
      <Button
        size={size}
        variant={paused ? "primary" : "secondary"}
        icon={paused ? "play" : "pause"}
        disabled={pause.isPending}
        onClick={() => pause.mutate(!paused)}
      >
        {paused ? t("Resume builds") : t("Pause builds")}
      </Button>
      <Button
        size={size}
        variant="danger"
        icon="stop"
        disabled={cancel.isPending || active === 0}
        title={active === 0 ? t("No builds are queued or running") : undefined}
        onClick={() => setConfirm(true)}
      >
        {t("Cancel all builds")}
      </Button>
      <Dialog
        open={confirm}
        onClose={() => setConfirm(false)}
        title={t("Cancel every build?")}
        description={t(
          "{queued} queued builds will not run and {running} running builds are stopped. Services keep their current version.",
          { queued: view.builds.queued, running: view.builds.running + view.builds.stack_builds },
        )}
      >
        <div className="flex flex-wrap justify-end gap-2">
          <Button quiet onClick={() => setConfirm(false)}>
            {t("Keep them")}
          </Button>
          <Button
            variant="danger-solid"
            disabled={cancel.isPending}
            onClick={() => cancel.mutate(undefined, { onSettled: () => setConfirm(false) })}
          >
            {t("Cancel all builds")}
          </Button>
        </div>
      </Dialog>
    </>
  );
}

/** A one-line summary of the build queue. */
export function queueSummary(t: Translate, view: LoadView): string {
  const { builds } = view;
  const parts: string[] = [];
  const running = builds.running + builds.stack_builds;
  parts.push(
    running === 1 ? t("1 build running") : t("{count} builds running", { count: running }),
  );
  parts.push(
    builds.queued === 1 ? t("1 waiting") : t("{count} waiting", { count: builds.queued }),
  );
  return parts.join(" · ");
}

/**
 * A banner above every page while the panel server is critical, builds are paused, or
 * builds are waiting for room. It links to the Load page, where all of it can be fixed.
 */
export function LoadBanner() {
  const t = useT();
  const { pathname } = useLocation();
  const { user } = useAuth();
  const load = useLoad();
  const { pause } = useLoadActions();
  const view = load.data;
  if (!view || pathname === "/load") {
    return null;
  }
  const { guard, builds } = view;
  let tone: "danger" | "amber" | null = null;
  let text: ReactNode = null;
  let action: ReactNode = (
    <Link to="/load" className="shrink-0 font-medium underline underline-offset-2">
      {t("Open load controls")}
    </Link>
  );
  if (guard.level === "critical" && guard.settings.enabled) {
    tone = "danger";
    text = (
      <>
        <strong className="font-semibold">{t("The server is running out of memory.")}</strong>{" "}
        {t("Builds are stopped so the panel keeps answering. Stop the heaviest service if it does not recover.")}
      </>
    );
  } else if (guard.settings.builds_paused) {
    tone = "amber";
    text = (
      <>
        <strong className="font-semibold">{t("Builds are paused.")}</strong>{" "}
        {builds.queued
          ? t("{count} builds wait until they are resumed.", { count: builds.queued })
          : t("New builds wait until they are resumed.")}
      </>
    );
    if (user?.role === "admin") {
      action = (
        <span className="flex shrink-0 items-center gap-3">
          <button
            type="button"
            className="font-semibold underline underline-offset-2"
            disabled={pause.isPending}
            onClick={() => pause.mutate(false)}
          >
            {t("Resume builds")}
          </button>
          {action}
        </span>
      );
    }
  } else if (builds.hold && builds.queued > 0) {
    tone = "amber";
    text = (
      <>
        <strong className="font-semibold">{t("The server is busy.")}</strong>{" "}
        {t("{count} builds wait until it has room.", { count: builds.queued })}
      </>
    );
  }
  if (!tone) {
    return null;
  }
  const cls =
    tone === "danger"
      ? "border-danger-line bg-danger-soft text-danger"
      : "border-amber-line bg-amber-soft text-amber";
  return (
    <div
      role={tone === "danger" ? "alert" : "status"}
      className={`flex flex-wrap items-center gap-x-3 gap-y-1 border-b px-4 py-2.5 text-sm sm:px-6 lg:px-8 ${cls}`}
    >
      <Icon name={tone === "danger" ? "alert" : "gauge"} size={16} className="shrink-0" />
      <p className="min-w-0 flex-1">{text}</p>
      {action}
    </div>
  );
}

type Sort = "cpu" | "memory";

interface Consumer {
  usage: Usage;
  service: Service | undefined;
  serverName: string;
  memoryShare: number | null;
}

/**
 * Services that use the most CPU or memory right now, with a Stop button: the first
 * thing to reach for when the server is overloaded.
 */
export function TopConsumers({ limit = 6, title }: { limit?: number; title?: ReactNode }) {
  const t = useT();
  const { user } = useAuth();
  const isAdmin = user?.role === "admin";
  const [sort, setSort] = useState<Sort>("cpu");
  const [stopping, setStopping] = useState<Consumer | null>(null);
  const overview = useQuery({
    queryKey: ["metrics-overview"],
    queryFn: () => api<Schemas["MetricsOverview"]>("/api/v1/metrics/overview"),
    refetchInterval: 10_000,
  });
  const services = useQuery({
    queryKey: ["services"],
    queryFn: () => api<Service[]>("/api/v1/services"),
    refetchInterval: 10_000,
  });
  const servers = useQuery({
    queryKey: ["servers"],
    queryFn: () => api<Server[]>("/api/v1/servers"),
    refetchInterval: 10_000,
  });

  const rows = useMemo(() => {
    const byId = new Map((services.data ?? []).map((service) => [service.id, service]));
    const serverById = new Map((servers.data ?? []).map((server) => [server.id, server]));
    return (overview.data?.services ?? [])
      .map((usage): Consumer => {
        const server = serverById.get(usage.server_id);
        const total = server?.memory_bytes ?? null;
        return {
          usage,
          service: byId.get(usage.service_id),
          serverName: server?.name ?? t("Unknown"),
          memoryShare:
            total && usage.memory_bytes != null ? (usage.memory_bytes / total) * 100 : null,
        };
      })
      .sort((a, b) =>
        sort === "cpu"
          ? (b.usage.cpu_percent ?? 0) - (a.usage.cpu_percent ?? 0)
          : (b.usage.memory_bytes ?? 0) - (a.usage.memory_bytes ?? 0),
      )
      .slice(0, limit);
  }, [overview.data, services.data, servers.data, sort, limit, t]);

  const loading = overview.isLoading;
  return (
    <section className="overflow-hidden rounded-card border border-line bg-surface shadow-card">
      <header className="flex flex-wrap items-start justify-between gap-3 px-5 pt-5 sm:px-6">
        <div className="min-w-0">
          <h2 className="text-[15px] leading-snug font-semibold text-ink">
            {title ?? t("Heaviest services")}
          </h2>
          <p className="mt-1 text-sm text-muted">
            {t("What uses the most right now. Stop one to give the server room.")}
          </p>
        </div>
        <Segmented<Sort>
          label={t("Sort services by")}
          value={sort}
          onChange={setSort}
          options={[
            ["cpu", t("CPU")],
            ["memory", t("Memory")],
          ]}
        />
      </header>
      <div className="pt-4">
        {loading ? (
          <div className="space-y-3 px-5 pb-5 sm:px-6">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        ) : rows.length === 0 ? (
          <Empty icon="activity" title={t("No readings yet")}>
            {t("Running services show up here within a few seconds.")}
          </Empty>
        ) : (
          <ul className="divide-y divide-line border-t border-line">
            {rows.map((row) => {
              const service = row.service;
              const kind = service?.kind ?? "application";
              const cpu = row.usage.cpu_percent ?? 0;
              return (
                <li
                  key={row.usage.service_id}
                  className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3 gap-y-2 px-5 py-3 sm:grid-cols-[auto_minmax(0,1fr)_150px_110px_auto] sm:px-6"
                >
                  <KindTile kind={kind} size={32} />
                  <Link to={`/services/${row.usage.service_id}`} className="group min-w-0">
                    <span className="block truncate text-sm font-medium text-ink group-hover:text-pine">
                      {row.usage.name}
                    </span>
                    <span className="block truncate text-xs text-muted">
                      {service
                        ? `${service.project_name} / ${service.environment_name} · ${row.serverName}`
                        : row.serverName}
                    </span>
                  </Link>
                  <div className="col-span-3 col-start-1 row-start-2 grid grid-cols-2 gap-3 sm:col-span-1 sm:col-start-auto sm:row-start-auto sm:contents">
                    <div className="min-w-0">
                      <div className="flex items-baseline justify-between text-xs">
                        <span className="text-muted">{t("CPU")}</span>
                        <span className="font-medium text-ink-soft tabular-nums">
                          {cpu.toFixed(cpu < 10 ? 1 : 0)}%
                        </span>
                      </div>
                      <div className="mt-1.5">
                        <ThresholdMeter
                          value={cpu}
                          warn={60}
                          danger={85}
                          marks={false}
                          label={t("CPU usage")}
                        />
                      </div>
                    </div>
                    <div className="min-w-0 text-right">
                      <p className="text-xs text-muted">{t("Memory")}</p>
                      <p className="text-sm font-medium text-ink-soft tabular-nums">
                        {formatBytes(row.usage.memory_bytes)}
                        {row.memoryShare != null ? (
                          <span className="ml-1 text-xs font-normal text-faint">
                            {round(row.memoryShare)}%
                          </span>
                        ) : null}
                      </p>
                    </div>
                  </div>
                  <span className="col-start-3 row-start-1 flex justify-end sm:col-start-auto sm:row-start-auto">
                    {isAdmin && service && service.status !== "stopped" ? (
                      <Button size="sm" variant="secondary" icon="stop" onClick={() => setStopping(row)}>
                        {t("Stop")}
                      </Button>
                    ) : (
                      <Link
                        to={`/services/${row.usage.service_id}`}
                        className={buttonClass({ variant: "ghost", size: "sm" })}
                        aria-label={t("Open {name}", { name: row.usage.name })}
                      >
                        <Icon name="chevronRight" size={14} />
                      </Link>
                    )}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
      </div>
      <StopServiceDialog consumer={stopping} onClose={() => setStopping(null)} />
    </section>
  );
}

function StopServiceDialog({
  consumer,
  onClose,
}: {
  consumer: Consumer | null;
  onClose: () => void;
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const service = consumer?.service;
  const stack = service?.kind === "compose";
  const stop = useMutation({
    mutationFn: (id: string) =>
      api(stack ? `/api/v1/services/${id}/stack/down` : `/api/v1/services/${id}/stop`, {
        method: "POST",
      }),
    onSuccess: () => {
      toast.success(t("{name} is stopping", { name: consumer?.usage.name ?? "" }));
      void queryClient.invalidateQueries({ queryKey: ["services"] });
      void queryClient.invalidateQueries({ queryKey: ["metrics-overview"] });
      onClose();
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not stop the service"))),
  });
  return (
    <Dialog
      open={consumer != null}
      onClose={onClose}
      title={t("Stop {name}?", { name: consumer?.usage.name ?? "" })}
      description={
        stack
          ? t("The stack's containers are removed with docker compose down; volumes and data are kept. Start it again from its page.")
          : t("Its container stops and visitors get errors until you start it again from its page. Data is kept.")
      }
    >
      <div className="flex flex-wrap justify-end gap-2">
        <Button quiet onClick={onClose}>
          {t("Keep it running")}
        </Button>
        <Button
          variant="danger-solid"
          icon="stop"
          disabled={!service || stop.isPending}
          onClick={() => service && stop.mutate(service.id)}
        >
          {stack ? t("Stop the stack") : t("Stop the service")}
        </Button>
      </div>
    </Dialog>
  );
}

/** The panel server at a glance: level, reasons, gauges, and the build queue. */
export function PanelLoadCard() {
  const t = useT();
  const load = useLoad();
  const view = load.data;
  return (
    <section className="overflow-hidden rounded-card border border-line bg-surface shadow-card">
      <header className="flex items-start justify-between gap-3 px-5 pt-5 sm:px-6">
        <div className="min-w-0">
          <h2 className="flex items-center gap-2 text-[15px] leading-snug font-semibold text-ink">
            {t("Panel server load")}
            {view ? <LevelBadge level={view.guard.level} /> : null}
          </h2>
          <p className="mt-1 text-sm text-muted">
            {view?.guard.since && view.guard.level !== "ok"
              ? t("Since {time}", { time: formatRelative(view.guard.since) })
              : t("Builds and apps share this server with the panel.")}
          </p>
        </div>
        <Link to="/load" className={buttonClass({ variant: "ghost", size: "sm" })}>
          {t("Details")}
          <Icon name="arrowRight" size={14} />
        </Link>
      </header>
      <div className="space-y-4 px-5 py-5 sm:px-6">
        {load.isLoading ? (
          <Skeleton className="h-16 w-full" />
        ) : !view ? (
          <p className="text-sm text-muted">
            {errorMessage(load.error, t("The panel did not answer."))}
          </p>
        ) : (
          <>
            {view.guard.reasons.length ? (
              <ul className="space-y-1 text-[13px]">
                {view.guard.reasons.map((reason) => (
                  <li
                    key={reason.code}
                    className={`flex items-start gap-1.5 ${view.guard.level === "critical" ? "text-danger" : "text-amber"}`}
                  >
                    <Icon name="alert" size={14} className="mt-0.5 shrink-0" />
                    {reasonText(t, reason)}
                  </li>
                ))}
              </ul>
            ) : null}
            <PressureGauges guard={view.guard} compact />
            <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2 border-t border-line pt-4">
              <p className="min-w-0 text-[13px] text-muted">
                <span className="font-medium text-ink-soft">{queueSummary(t, view)}</span>
                {holdText(t, view.builds.hold) ? (
                  <span className="block text-amber">{holdText(t, view.builds.hold)}</span>
                ) : null}
              </p>
              <span className="flex flex-wrap gap-2">
                <BuildActions view={view} size="sm" />
              </span>
            </div>
          </>
        )}
      </div>
    </section>
  );
}
