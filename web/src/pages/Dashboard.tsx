import { useQuery } from "@tanstack/react-query";
import { useMemo, useState, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { api } from "../api";
import { useAuth } from "../auth";
import { Icon, type IconName } from "../components/icons";
import {
  Alert,
  Badge,
  buttonClass,
  Card,
  Empty,
  Meter,
  PageHeader,
  Segmented,
  Skeleton,
  StatusDot,
  toneOf,
} from "../components/ui";
import { errorMessage, formatBytes, formatRelative, formatTime } from "../format";
import type { Schemas } from "../types";

type Server = Schemas["ServerView"];
type Service = Schemas["ServiceSummary"];
type Usage = Schemas["ServerUsage"];
type Event = Schemas["EventView"];
type AlertRule = Schemas["AlertView"];
type Project = Schemas["ProjectView"];

type Filter = "all" | "running" | "issues" | "stopped";

const LIVE = 10_000;
const PAGE = 8;

// Failed first, then work in flight, then healthy, then idle.
const statusOrder: Record<string, number> = {
  failed: 0,
  error: 0,
  partial: 1,
  rolled_back: 1,
  deploying: 2,
  building: 2,
  queued: 2,
  running: 3,
  stopped: 4,
};

function isIssue(status: string) {
  const tone = toneOf(status);
  return tone === "danger" || tone === "warning";
}

export function DashboardPage() {
  const { user } = useAuth();
  const isAdmin = user?.role === "admin";

  const servers = useQuery({
    queryKey: ["servers"],
    queryFn: () => api<Server[]>("/api/v1/servers"),
    refetchInterval: LIVE,
  });
  const services = useQuery({
    queryKey: ["services"],
    queryFn: () => api<Service[]>("/api/v1/services"),
    refetchInterval: LIVE,
  });
  const overview = useQuery({
    queryKey: ["metrics-overview"],
    queryFn: () => api<Schemas["MetricsOverview"]>("/api/v1/metrics/overview"),
    refetchInterval: LIVE,
  });
  const projects = useQuery({
    queryKey: ["projects"],
    queryFn: () => api<Project[]>("/api/v1/projects"),
  });
  const alerts = useQuery({
    queryKey: ["alerts"],
    queryFn: () => api<AlertRule[]>("/api/v1/alerts"),
    refetchInterval: 30_000,
  });
  const events = useQuery({
    queryKey: ["events", "recent"],
    queryFn: () => api<Event[]>("/api/v1/events?limit=8"),
    refetchInterval: 30_000,
  });

  const serverList = servers.data ?? [];
  const serviceList = services.data ?? [];
  const alertList = alerts.data ?? [];
  const usage = new Map((overview.data?.servers ?? []).map((row) => [row.server_id, row]));
  const serverName = new Map(serverList.map((server) => [server.id, server.name]));

  const online = serverList.filter((server) => server.status === "online").length;
  const offline = serverList.filter((server) => server.status === "offline");
  const running = serviceList.filter((service) => service.status === "running").length;
  const failing = serviceList.filter((service) => isIssue(service.status));
  const firing = alertList.filter((rule) => rule.enabled && rule.state === "firing");
  const pendingAlerts = alertList.filter((rule) => rule.enabled && rule.state === "pending");
  const projectCount = projects.data?.length ?? new Set(serviceList.map((s) => s.project_id)).size;
  const activeProjects = new Set(serviceList.map((service) => service.project_id)).size;

  const loading = servers.isLoading || services.isLoading;
  const issues: ReactNode[] = [];
  if (failing.length) {
    issues.push(
      <span key="svc">
        {failing.length} {failing.length === 1 ? "service needs" : "services need"} a look
      </span>,
    );
  }
  if (offline.length) {
    issues.push(
      <Link key="srv" to="/servers">
        {offline.length === 1 ? `${offline[0]?.name} is offline` : `${offline.length} servers are offline`}
      </Link>,
    );
  }
  if (firing.length) {
    issues.push(
      <Link key="alr" to="/alerts">
        {firing.length} {firing.length === 1 ? "alert is" : "alerts are"} firing
      </Link>,
    );
  }

  return (
    <>
      <PageHeader
        title="Overview"
        description={
          loading
            ? "Checking your servers…"
            : `${serviceList.length} ${serviceList.length === 1 ? "service" : "services"} on ${serverList.length} ${serverList.length === 1 ? "server" : "servers"}. Refreshes every 10 seconds.`
        }
        actions={
          isAdmin ? (
            <>
              <Link to="/templates" className={buttonClass({ variant: "secondary" })}>
                <Icon name="layers" />
                Deploy a template
              </Link>
              <Link to="/projects" className={buttonClass()}>
                <Icon name="plus" />
                New project
              </Link>
            </>
          ) : null
        }
      />

      {services.error || servers.error ? (
        <Alert tone="error" title="Couldn't load the overview">
          {errorMessage(services.error ?? servers.error, "The panel did not answer.")}
        </Alert>
      ) : issues.length ? (
        <Alert
          tone={offline.length || firing.length ? "error" : "warning"}
          title={issues.length === 1 ? "One thing needs attention" : `${issues.length} things need attention`}
        >
          <span className="flex flex-wrap gap-x-1.5">
            {issues.map((issue, index) => (
              <span key={index}>
                {issue}
                {index < issues.length - 1 ? " ·" : "."}
              </span>
            ))}
          </span>
        </Alert>
      ) : null}

      <div className="grid grid-cols-2 gap-3 sm:gap-4 xl:grid-cols-4">
        <StatTile
          to="/servers"
          icon="server"
          label="Servers online"
          loading={servers.isLoading}
          value={online}
          total={serverList.length}
          caption={
            offline.length
              ? `${offline.length} offline`
              : serverList.length
                ? "All reachable"
                : "None added yet"
          }
          warn={offline.length > 0}
        />
        <StatTile
          to="/projects"
          icon="rocket"
          label="Services running"
          loading={services.isLoading}
          value={running}
          total={serviceList.length}
          caption={
            failing.length
              ? `${failing.length} with problems`
              : serviceList.length - running
                ? `${serviceList.length - running} stopped`
                : serviceList.length
                  ? "Everything is up"
                  : "Nothing deployed yet"
          }
          warn={failing.length > 0}
        />
        <StatTile
          to="/projects"
          icon="folder"
          label="Projects"
          loading={projects.isLoading && services.isLoading}
          value={projectCount}
          caption={`${activeProjects} with running services`}
        />
        <StatTile
          to="/alerts"
          icon="bell"
          label="Alerts firing"
          loading={alerts.isLoading}
          value={firing.length}
          caption={
            firing[0]
              ? firing.length > 1
                ? `${firing[0].name} and ${firing.length - 1} more`
                : firing[0].name
              : alertList.length
                ? `${alertList.length} ${alertList.length === 1 ? "rule" : "rules"}${pendingAlerts.length ? ` · ${pendingAlerts.length} pending` : ""}`
                : "No rules yet"
          }
          warn={firing.length > 0}
        />
      </div>

      <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_340px] xl:grid-cols-[minmax(0,1fr)_380px]">
        <ServicesCard
          services={serviceList}
          serverName={serverName}
          loading={services.isLoading}
          isAdmin={isAdmin}
        />
        <div className="grid gap-6">
          <ServersCard servers={serverList} usage={usage} loading={servers.isLoading} />
          <ActivityCard
            events={events.data ?? []}
            loading={events.isLoading}
            serverName={serverName}
          />
        </div>
      </div>
    </>
  );
}

function StatTile({
  to,
  icon,
  label,
  value,
  total,
  caption,
  loading,
  warn = false,
}: {
  to: string;
  icon: IconName;
  label: string;
  value: number;
  total?: number;
  caption: string;
  loading: boolean;
  warn?: boolean;
}) {
  const ratio = total ? (value / total) * 100 : null;
  return (
    <Link
      to={to}
      className="group min-w-0 rounded-card border border-line bg-surface p-4 shadow-card sm:p-5 transition-colors hover:border-line-strong"
    >
      <div className="flex items-center justify-between text-[13px] font-medium text-muted">
        <span className="flex min-w-0 items-center gap-2">
          <span className="hidden size-7 shrink-0 items-center justify-center rounded-alert bg-sunken text-ink-soft sm:inline-flex">
            <Icon name={icon} size={15} />
          </span>
          <span className="truncate">{label}</span>
        </span>
        <Icon
          name="arrowRight"
          size={14}
          className="-translate-x-1 text-faint opacity-0 transition group-hover:translate-x-0 group-hover:opacity-100"
        />
      </div>
      {loading ? (
        <Skeleton className="mt-4 h-8 w-20" />
      ) : (
        <p className="mt-4 flex items-baseline gap-1.5 tabular-nums">
          <span className="text-[28px] leading-none font-semibold tracking-[-0.02em] text-ink">
            {value}
          </span>
          {total != null ? <span className="text-sm text-muted">of {total}</span> : null}
        </p>
      )}
      {total != null ? (
        <Meter
          value={loading ? null : ratio}
          label={`${label} ratio`}
          className="mt-3 [&>div]:bg-pine!"
        />
      ) : null}
      <p
        className={`mt-2.5 flex items-center gap-1.5 text-[13px] ${warn ? "font-medium text-danger" : "text-muted"}`}
      >
        {warn ? <Icon name="alert" size={14} /> : null}
        {loading ? " " : caption}
      </p>
    </Link>
  );
}

function ServicesCard({
  services,
  serverName,
  loading,
  isAdmin,
}: {
  services: Service[];
  serverName: Map<string, string>;
  loading: boolean;
  isAdmin: boolean;
}) {
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState(false);

  const counts = useMemo(
    () => ({
      all: services.length,
      running: services.filter((s) => s.status === "running").length,
      issues: services.filter((s) => isIssue(s.status)).length,
      stopped: services.filter((s) => s.status === "stopped").length,
    }),
    [services],
  );

  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return services
      .filter((service) => {
        if (filter === "running" && service.status !== "running") return false;
        if (filter === "issues" && !isIssue(service.status)) return false;
        if (filter === "stopped" && service.status !== "stopped") return false;
        if (!needle) return true;
        return [service.name, service.project_name, service.environment_name, service.kind]
          .join(" ")
          .toLowerCase()
          .includes(needle);
      })
      .sort(
        (a, b) =>
          (statusOrder[a.status] ?? 3) - (statusOrder[b.status] ?? 3) ||
          a.project_name.localeCompare(b.project_name) ||
          a.name.localeCompare(b.name),
      );
  }, [services, filter, query]);

  const shown = expanded ? visible : visible.slice(0, PAGE);

  if (!loading && services.length === 0) {
    return (
      <Card title="Services">
        <Empty
          icon="rocket"
          title="Deploy your first service"
          action={
            isAdmin ? (
              <>
                <Link to="/templates" className={buttonClass({ variant: "secondary", size: "sm" })}>
                  Browse templates
                </Link>
                <Link to="/projects" className={buttonClass({ size: "sm" })}>
                  Create a project
                </Link>
              </>
            ) : null
          }
        >
          Apps, databases and Compose stacks appear here once a project has them.
        </Empty>
      </Card>
    );
  }

  return (
    <Card
      title="Services"
      description="Apps, databases and stacks across every project."
      flush
      footer={
        <>
          <span>
            {loading
              ? "Loading…"
              : `Showing ${shown.length} of ${visible.length}${visible.length !== services.length ? ` (${services.length} total)` : ""}`}
          </span>
          <span className="flex items-center gap-3">
            {visible.length > PAGE ? (
              <button
                type="button"
                className="font-medium text-ink-soft hover:text-ink"
                onClick={() => setExpanded((value) => !value)}
              >
                {expanded ? "Show fewer" : `Show all ${visible.length}`}
              </button>
            ) : null}
            <Link to="/projects" className="inline-flex items-center gap-1 font-medium text-pine">
              Projects <Icon name="arrowRight" size={14} />
            </Link>
          </span>
        </>
      }
    >
      <div className="flex flex-wrap items-center gap-2 border-b border-line px-5 pb-4 sm:px-6">
        <label className="relative min-w-0 flex-1 basis-48">
          <span className="sr-only">Search services</span>
          <Icon
            name="search"
            className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-faint"
          />
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search by name, project or kind"
            className="h-8 w-full rounded-control border border-line-strong bg-surface pr-3 pl-8 text-sm text-ink outline-none placeholder:text-faint focus:border-pine focus:shadow-[0_0_0_3px_color-mix(in_srgb,var(--pine)_14%,transparent)]"
          />
        </label>
        <div className="max-w-full overflow-x-auto">
          <Segmented<Filter>
            label="Filter services by status"
            value={filter}
            onChange={setFilter}
            options={[
              ["all", "All", counts.all],
              ["running", "Running", counts.running],
              ["issues", "Issues", counts.issues],
              ["stopped", "Stopped", counts.stopped],
            ]}
          />
        </div>
      </div>
      {loading ? (
        <ul className="divide-y divide-line">
          {Array.from({ length: 4 }, (_, index) => (
            <li key={index} className="flex items-center gap-3 px-5 py-3.5 sm:px-6">
              <Skeleton className="size-2 rounded-full" />
              <div className="flex-1 space-y-1.5">
                <Skeleton className="h-4 w-40" />
                <Skeleton className="h-3 w-56 max-w-full" />
              </div>
              <Skeleton className="h-5 w-16 rounded-full" />
            </li>
          ))}
        </ul>
      ) : visible.length === 0 ? (
        <Empty icon="search" title="No matching services">
          Try another name or clear the filter.
        </Empty>
      ) : (
        <ul className="divide-y divide-line">
          {shown.map((service) => (
            <li key={service.id}>
              <Link
                to={`/services/${service.id}`}
                className="group grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3 px-5 py-3 transition-colors hover:bg-paper sm:px-6 md:grid-cols-[auto_minmax(0,1fr)_88px_120px_118px]"
              >
                <StatusDot status={service.status} />
                <span className="min-w-0">
                  <span className="block truncate text-sm font-medium text-ink group-hover:text-pine">
                    {service.name}
                  </span>
                  <span className="block truncate text-[13px] text-muted">
                    {service.project_name}
                    <span className="px-1 text-faint">/</span>
                    {service.environment_name}
                  </span>
                </span>
                <span className="hidden justify-self-start md:block">
                  <Badge value={service.kind} tone="outline" />
                </span>
                <span className="hidden truncate text-[13px] text-muted md:flex md:items-center md:gap-1.5">
                  <Icon name="server" size={14} className="shrink-0 text-faint" />
                  <span className="truncate">{serverName.get(service.server_id) ?? "Unknown"}</span>
                </span>
                <span className="flex items-center justify-end gap-2">
                  <Badge value={service.status} />
                  <Icon
                    name="chevronRight"
                    size={14}
                    className="hidden text-faint transition-transform group-hover:translate-x-0.5 sm:block"
                  />
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

function ServersCard({
  servers,
  usage,
  loading,
}: {
  servers: Server[];
  usage: Map<string, Usage>;
  loading: boolean;
}) {
  return (
    <Card
      title="Servers"
      flush
      actions={
        <Link to="/monitoring" className={buttonClass({ variant: "ghost", size: "sm" })}>
          Monitoring
          <Icon name="arrowRight" size={14} />
        </Link>
      }
    >
      {loading ? (
        <div className="space-y-3 px-5 pb-5 sm:px-6">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      ) : servers.length === 0 ? (
        <Empty icon="server" title="No servers yet">
          <Link to="/servers">Add a server</Link> to start deploying.
        </Empty>
      ) : (
        <ul className="divide-y divide-line border-t border-line">
          {servers.map((server) => {
            const row = usage.get(server.id);
            return (
              <li key={server.id} className="px-5 py-4 sm:px-6">
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0">
                    <p className="flex items-center gap-2 text-sm font-medium text-ink">
                      <StatusDot status={server.status} />
                      <span className="truncate">{server.name}</span>
                    </p>
                    <p className="mt-0.5 truncate pl-4 font-mono text-xs text-muted">
                      {[server.public_ip, server.hostname].filter(Boolean).join(" · ") ||
                        server.kind}
                    </p>
                  </div>
                  <Badge value={server.status} />
                </div>
                {row && server.status === "online" ? (
                  <dl className="mt-3 grid grid-cols-3 gap-3 pl-4">
                    <Gauge label="CPU" value={row.cpu_percent} />
                    <Gauge label="Memory" value={row.memory_percent} />
                    <Gauge label="Disk" value={row.disk_percent} />
                  </dl>
                ) : (
                  <p className="mt-2 pl-4 text-xs text-muted">
                    {server.last_heartbeat_at
                      ? `Last seen ${formatRelative(server.last_heartbeat_at)}`
                      : server.memory_bytes
                        ? `${server.cpu_count ?? "?"} vCPU · ${formatBytes(server.memory_bytes)}`
                        : "No samples yet"}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}

function Gauge({ label, value }: { label: string; value: number | null | undefined }) {
  return (
    <div className="min-w-0">
      <div className="flex items-baseline justify-between gap-1 text-xs">
        <dt className="text-muted">{label}</dt>
        <dd className="font-medium text-ink-soft tabular-nums">
          {value == null ? "—" : `${Math.round(value)}%`}
        </dd>
      </div>
      <Meter value={value} label={`${label} usage`} className="mt-1.5" />
    </div>
  );
}

const infoStyle: [string, IconName] = ["border-line bg-sunken text-ink-soft", "info"];

const severityStyle: Record<string, [string, IconName]> = {
  critical: ["border-danger-line bg-danger-soft text-danger", "alert"],
  error: ["border-danger-line bg-danger-soft text-danger", "alert"],
  warning: ["border-amber-line bg-amber-soft text-amber", "alert"],
  info: ["border-line bg-sunken text-ink-soft", "info"],
};

function ActivityCard({
  events,
  loading,
  serverName,
}: {
  events: Event[];
  loading: boolean;
  serverName: Map<string, string>;
}) {
  return (
    <Card
      title="Recent activity"
      flush
      actions={
        <Link to="/logs" className={buttonClass({ variant: "ghost", size: "sm" })}>
          Logs
          <Icon name="arrowRight" size={14} />
        </Link>
      }
    >
      {loading ? (
        <div className="space-y-3 px-5 pb-5 sm:px-6">
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-full" />
        </div>
      ) : events.length === 0 ? (
        <Empty icon="history" title="All quiet">
          Deploys, backups and alerts will show up here.
        </Empty>
      ) : (
        <ol className="border-t border-line px-5 py-2 sm:px-6">
          {events.map((event, index) => {
            const [cls, icon] = severityStyle[event.severity] ?? infoStyle;
            return (
              <li key={event.id} className="relative flex gap-3 py-2.5">
                {index < events.length - 1 ? (
                  <span
                    className="absolute top-9 bottom-0 left-[13px] w-px bg-line"
                    aria-hidden="true"
                  />
                ) : null}
                <span
                  className={`relative inline-flex size-7 shrink-0 items-center justify-center rounded-full border ${cls}`}
                >
                  <Icon name={icon} size={14} />
                </span>
                <div className="min-w-0 flex-1">
                  <p className="flex items-baseline justify-between gap-2">
                    <span className="truncate text-sm font-medium text-ink">{event.title}</span>
                    <time
                      dateTime={event.created_at}
                      title={formatTime(event.created_at)}
                      className="shrink-0 text-xs text-faint tabular-nums"
                    >
                      {formatRelative(event.created_at)}
                    </time>
                  </p>
                  <p className="line-clamp-2 text-[13px] text-muted">
                    {event.server_id && serverName.get(event.server_id) ? (
                      <span className="text-ink-soft">{serverName.get(event.server_id)} · </span>
                    ) : null}
                    {event.message}
                  </p>
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </Card>
  );
}
