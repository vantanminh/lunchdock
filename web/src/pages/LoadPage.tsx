import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { useAuth } from "../auth";
import { Icon } from "../components/icons";
import {
  BuildActions,
  holdText,
  LevelBadge,
  PressureGauges,
  reasonText,
  TopConsumers,
  useLoad,
  type LoadView,
} from "../components/load";
import {
  Alert,
  Badge,
  Button,
  Card,
  CodeBlock,
  DataTable,
  Field,
  PageHeader,
  QuickPick,
  Skeleton,
  Td,
  Th,
  TextInput,
} from "../components/ui";
import { errorMessage, formatBytes, formatRelative, formatTime } from "../format";
import { msg, useT } from "../i18n";

const roleLabel: Record<string, string> = {
  panel: msg("Panel"),
  database: msg("Panel database"),
  proxy: msg("Traefik"),
};

export function LoadPage() {
  const t = useT();
  const { user } = useAuth();
  const isAdmin = user?.role === "admin";
  const load = useLoad();
  const view = load.data;

  return (
    <>
      <PageHeader
        title={t("Load")}
        description={t(
          "Keeps the panel reachable when builds or traffic overload the server: new builds wait while it is busy, and builds stop before memory runs out.",
        )}
        actions={view ? <BuildActions view={view} /> : null}
      />

      {load.error && !view ? (
        <Alert tone="error" title={t("Couldn't load the server's load")}>
          {errorMessage(load.error, t("The panel did not answer."))}
        </Alert>
      ) : null}

      {!view ? (
        <div className="grid gap-6">
          <Skeleton className="h-40 w-full rounded-card" />
          <Skeleton className="h-56 w-full rounded-card" />
        </div>
      ) : (
        <>
          <StatusCard view={view} />
          <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_360px]">
            <TopConsumers limit={8} />
            <BuildsCard view={view} />
          </div>
          <ProtectionCard view={view} />
          {isAdmin ? <SettingsCard view={view} /> : null}
          <RescueCard />
        </>
      )}
    </>
  );
}

function StatusCard({ view }: { view: LoadView }) {
  const t = useT();
  const { guard } = view;
  const tone =
    guard.level === "critical"
      ? "border-danger-line bg-danger-soft text-danger"
      : guard.level === "busy"
        ? "border-amber-line bg-amber-soft text-amber"
        : "border-pine-line bg-pine-soft text-pine";
  const headline =
    guard.level === "critical"
      ? t("The server is running out of memory")
      : guard.level === "busy"
        ? t("The server is busy")
        : t("The server has room");
  const effect = !guard.settings.enabled
    ? t("The guard is off: it watches but does not hold or stop builds.")
    : guard.level === "critical"
      ? t("Running builds are stopped so the panel keeps answering.")
      : guard.level === "busy"
        ? t("New builds wait in the queue until it has room again.")
        : t("Builds start as soon as they are queued.");
  return (
    <section className="overflow-hidden rounded-card border border-line bg-surface shadow-card">
      <div className="flex flex-wrap items-start gap-4 px-5 pt-5 sm:px-6">
        <span
          className={`inline-flex size-11 shrink-0 items-center justify-center rounded-card border ${tone}`}
        >
          <Icon name={guard.level === "ok" ? "check" : "gauge"} size={22} />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="flex flex-wrap items-center gap-2 text-lg leading-snug font-semibold text-ink">
            {headline}
            <LevelBadge level={guard.level} />
            {guard.settings.enabled ? null : <Badge value={msg("Guard off")} tone="outline" />}
          </h2>
          <p className="mt-1 text-sm text-muted">{effect}</p>
          {guard.reasons.length ? (
            <ul className="mt-2 space-y-1 text-sm">
              {guard.reasons.map((reason) => (
                <li key={reason.code} className="flex items-start gap-1.5 text-ink-soft">
                  <Icon name="alert" size={14} className="mt-1 shrink-0 text-amber" />
                  {reasonText(t, reason)}
                </li>
              ))}
            </ul>
          ) : null}
        </div>
        <p className="text-right text-xs text-faint">
          {guard.since ? (
            <span className="block">
              {t("Level since {time}", { time: formatRelative(guard.since) })}
            </span>
          ) : null}
          <span title={formatTime(guard.sampled_at)}>
            {t("Read {time}", { time: formatRelative(guard.sampled_at) })}
          </span>
        </p>
      </div>
      <div className="px-5 py-5 sm:px-6">
        {guard.error && guard.pressure ? (
          <div className="mb-4">
            <Alert tone="warning">
              {t("The last reading failed: {error}", { error: guard.error })}
            </Alert>
          </div>
        ) : null}
        <PressureGauges guard={guard} />
        <p className="mt-4 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted">
          <span className="flex items-center gap-1.5">
            <span className="h-3 w-0.5 rounded-full bg-amber/70" />
            {t("New builds wait past this line")}
          </span>
          <span className="flex items-center gap-1.5">
            <span className="h-3 w-0.5 rounded-full bg-danger/70" />
            {t("Running builds stop past this line")}
          </span>
        </p>
      </div>
    </section>
  );
}

function BuildsCard({ view }: { view: LoadView }) {
  const t = useT();
  const { builds, guard } = view;
  const hold = holdText(t, builds.hold);
  const rows: [string, ReactNode][] = [
    [t("Running builds"), builds.running],
    [t("Stack builds running"), builds.stack_builds],
    [t("Waiting in the queue"), builds.queued],
    [
      t("Oldest waiting"),
      builds.oldest_queued_at ? formatRelative(builds.oldest_queued_at) : "—",
    ],
    [t("Builds at the same time"), builds.concurrency],
    [
      t("Stopped by the guard"),
      guard.stopped_builds
        ? `${guard.stopped_builds} · ${formatRelative(guard.last_stopped_at)}`
        : "0",
    ],
  ];
  return (
    <Card
      title={t("Build queue")}
      description={t("Builds run on the panel server, so they are what the guard holds and stops.")}
    >
      {hold ? (
        <div className="mb-4">
          <Alert tone={builds.hold === "critical" ? "error" : "warning"}>{hold}</Alert>
        </div>
      ) : null}
      <dl className="divide-y divide-line text-sm">
        {rows.map(([label, value]) => (
          <div key={label} className="flex items-baseline justify-between gap-3 py-2">
            <dt className="text-muted">{label}</dt>
            <dd className="font-medium text-ink tabular-nums">{value}</dd>
          </div>
        ))}
      </dl>
      <p className="mt-3 text-xs text-muted">
        {t("A build held only because of CPU starts anyway after 20 minutes.")}
      </p>
    </Card>
  );
}

function ProtectionCard({ view }: { view: LoadView }) {
  const t = useT();
  const unprotected = view.protection.filter((item) => item.present && !item.protected);
  return (
    <Card
      title={t("Panel protection")}
      description={t(
        "The panel, its database and Traefik get more CPU than apps, and the kernel kills apps before them when memory runs out.",
      )}
      flush
    >
      {view.protection.length === 0 ? (
        <p className="px-5 pb-5 text-sm text-muted sm:px-6">
          {t("Checking the panel's containers…")}
        </p>
      ) : (
        <>
          <DataTable
            head={
              <>
                <Th>{t("Container")}</Th>
                <Th>{t("Protection")}</Th>
                <Th className="text-right">{t("OOM score")}</Th>
                <Th className="text-right">{t("CPU weight")}</Th>
                <Th className="text-right">{t("Reserved memory")}</Th>
              </>
            }
          >
            {view.protection.map((item) => (
              <tr key={item.name}>
                <Td>
                  <span className="block font-medium text-ink">
                    {t(roleLabel[item.role] ?? item.role)}
                  </span>
                  <span className="font-mono text-xs text-muted">{item.name}</span>
                </Td>
                <Td>
                  {!item.present ? (
                    <Badge value={msg("Not on this server")} tone="outline" />
                  ) : item.protected ? (
                    <Badge value={msg("Protected")} tone="success" />
                  ) : (
                    <Badge value={msg("Partly protected")} tone="warning" />
                  )}
                </Td>
                <Td className="text-right tabular-nums">
                  {item.present ? item.oom_score_adj : "—"}
                </Td>
                <Td className="text-right tabular-nums">
                  {item.present ? item.cpu_shares || 1024 : "—"}
                </Td>
                <Td className="text-right tabular-nums">
                  {item.present && item.memory_reservation_bytes
                    ? formatBytes(item.memory_reservation_bytes)
                    : "—"}
                </Td>
              </tr>
            ))}
          </DataTable>
          {unprotected.length ? (
            <div className="px-5 py-4 sm:px-6">
              <Alert tone="info" title={t("Finish protecting the panel")}>
                {t(
                  "Docker cannot lower the OOM score of a running container. The panel gets it with the next update under System → Updates; Traefik and the database get it when the installer creates their containers. Their CPU weight is already raised.",
                )}
              </Alert>
            </div>
          ) : null}
        </>
      )}
    </Card>
  );
}

const RESERVE_PRESETS = [
  { label: "10%", value: "10" },
  { label: "15%", value: "15" },
  { label: "20%", value: "20" },
  { label: "25%", value: "25" },
] as const;

const CPU_PRESETS = [
  { label: "80%", value: "80" },
  { label: "90%", value: "90" },
  { label: "95%", value: "95" },
  { label: "100%", value: "100" },
] as const;

const CONCURRENCY_PRESETS = [
  { label: "1", value: "1" },
  { label: "2", value: "2" },
  { label: "3", value: "3" },
  { label: "4", value: "4" },
] as const;

function SettingsCard({ view }: { view: LoadView }) {
  const t = useT();
  const queryClient = useQueryClient();
  const settings = view.guard.settings;
  const [enabled, setEnabled] = useState(settings.enabled);
  const [reserve, setReserve] = useState(String(settings.memory_reserve_percent));
  const [cpu, setCpu] = useState(String(settings.cpu_percent));
  const [concurrency, setConcurrency] = useState(String(view.builds.concurrency));

  // Follow changes made elsewhere until the form is edited.
  const saved = `${settings.enabled}/${settings.memory_reserve_percent}/${settings.cpu_percent}/${view.builds.concurrency}`;
  const [baseline, setBaseline] = useState(saved);
  useEffect(() => {
    if (saved !== baseline) {
      setBaseline(saved);
      setEnabled(settings.enabled);
      setReserve(String(settings.memory_reserve_percent));
      setCpu(String(settings.cpu_percent));
      setConcurrency(String(view.builds.concurrency));
    }
  }, [saved, baseline, settings, view.builds.concurrency]);

  const save = useMutation({
    mutationFn: () =>
      api<LoadView>("/api/v1/load/settings", {
        method: "PUT",
        body: JSON.stringify({
          enabled,
          memory_reserve_percent: Number(reserve),
          cpu_percent: Number(cpu),
          build_concurrency: Number(concurrency),
        }),
      }),
    onSuccess: (next) => {
      queryClient.setQueryData(["load"], next);
      toast.success(t("Load guard settings saved"));
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not save the load guard settings"))),
  });
  const reserveNumber = Number(reserve);
  return (
    <Card
      title={t("Guard settings")}
      description={t("The defaults suit most servers. Raise the memory reserve on small servers that run out often.")}
    >
      <form
        className="grid gap-5 md:grid-cols-3"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field
          label={t("Memory reserve (%)")}
          hint={t("New builds wait below this much free memory; running builds stop below {half}%.", {
            half: Number.isFinite(reserveNumber) ? Math.max(reserveNumber / 2, 3) : "—",
          })}
          presets={
            <QuickPick label={t("Memory reserve presets")} options={RESERVE_PRESETS} value={reserve} onPick={setReserve} />
          }
        >
          <TextInput
            type="number"
            min={5}
            max={50}
            required
            value={reserve}
            onChange={(event) => setReserve(event.target.value)}
          />
        </Field>
        <Field
          label={t("CPU limit (%)")}
          hint={t("New builds wait while CPU stays above this. 100% never holds for CPU.")}
          presets={<QuickPick label={t("CPU limit presets")} options={CPU_PRESETS} value={cpu} onPick={setCpu} />}
        >
          <TextInput
            type="number"
            min={50}
            max={100}
            required
            value={cpu}
            onChange={(event) => setCpu(event.target.value)}
          />
        </Field>
        <Field
          label={t("Builds at the same time")}
          hint={t("Per server. One is safest on a small server.")}
          presets={
            <QuickPick
              label={t("Builds at the same time presets")}
              options={CONCURRENCY_PRESETS}
              value={concurrency}
              onPick={setConcurrency}
            />
          }
        >
          <TextInput
            type="number"
            min={1}
            max={8}
            required
            value={concurrency}
            onChange={(event) => setConcurrency(event.target.value)}
          />
        </Field>
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line pt-4 md:col-span-3">
          <label className="flex items-center gap-2 text-sm text-ink">
            <input
              type="checkbox"
              checked={enabled}
              onChange={(event) => setEnabled(event.target.checked)}
            />
            {t("Hold and stop builds when the server is overloaded")}
          </label>
          <Button type="submit" disabled={save.isPending}>
            {t("Save settings")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

function RescueCard() {
  const t = useT();
  const direct = `http://${window.location.hostname}:3000`;
  return (
    <Card
      title={t("If the panel stops answering")}
      description={t("Traffic to your apps goes through Traefik. The panel also listens on its own port, which skips Traefik.")}
    >
      <ol className="list-decimal space-y-2 pl-5 text-sm text-ink-soft">
        <li>
          {t("Open the panel on its direct port:")}
          <div className="mt-1.5 max-w-md">
            <CodeBlock copy>{direct}</CodeBlock>
          </div>
        </li>
        <li>{t("Pause builds and cancel the running ones on this page.")}</li>
        <li>{t("Stop the service at the top of the CPU or memory list.")}</li>
        <li>
          {t("Still stuck? Over SSH, list the heaviest containers and stop the one at the top:")}
          <div className="mt-1.5 max-w-md">
            <CodeBlock copy>{"docker stats --no-stream --format '{{.Name}} {{.CPUPerc}} {{.MemUsage}}'"}</CodeBlock>
          </div>
        </li>
      </ol>
    </Card>
  );
}
