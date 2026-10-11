import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { errorMessage, formatTime } from "../format";
import { useT } from "../i18n";
import { MAINTENANCE_WINDOWS, UPDATE_CHANNELS } from "../presets";
import type { Schemas } from "../types";
import { Badge, Button, Card, Empty, Field, PickField, QuickPick, TextInput } from "./ui";

type Overview = Schemas["UpdateOverview"];

/** Polls /api/health until the panel answers with a version other than `from`. */
function useRestartWatch(from: string | null, onBack: (version: string) => void) {
  useEffect(() => {
    if (!from) {
      return;
    }
    let stopped = false;
    const timer = window.setInterval(async () => {
      try {
        const response = await fetch("/api/health", { cache: "no-store" });
        if (!response.ok) {
          return;
        }
        const health = (await response.json()) as { version?: string };
        if (!stopped && health.version && health.version !== from) {
          stopped = true;
          onBack(health.version);
        }
      } catch {
        // The panel is restarting.
      }
    }, 3000);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [from, onBack]);
}

export function UpdatesCard() {
  const queryClient = useQueryClient();
  const [restartingFrom, setRestartingFrom] = useState<string | null>(null);
  const overview = useQuery({
    queryKey: ["updates"],
    queryFn: () => api<Overview>("/api/v1/system/updates"),
    refetchInterval: (query) =>
      restartingFrom || query.state.data?.history.some((entry) => entry.status === "running")
        ? 4000
        : false,
    retry: restartingFrom ? false : 3,
  });
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["updates"] });

  useRestartWatch(restartingFrom, (version) => {
    setRestartingFrom(null);
    toast.success(`The panel is back on ${version}`);
    void queryClient.invalidateQueries();
  });

  const check = useMutation({
    mutationFn: () =>
      api<Schemas["UpdateCheck"]>("/api/v1/system/updates/check", { method: "POST" }),
    onSuccess: (result) => {
      void refresh();
      if (result.error) {
        toast.error(result.error);
      } else if (result.latest) {
        toast.success(`Rudder ${result.latest.version} is available`);
      } else {
        toast.success("Rudder is up to date");
      }
    },
    onError: (error) => toast.error(errorMessage(error, "Could not check for updates")),
  });
  const apply = useMutation({
    mutationFn: () =>
      api<Schemas["UpdateStarted"]>("/api/v1/system/updates/apply", { method: "POST" }),
    onSuccess: () => {
      toast.success("Update started: backing up, then the panel restarts");
      setRestartingFrom(overview.data?.current_version ?? null);
      void refresh();
    },
    onError: (error) => toast.error(errorMessage(error, "Could not start the update")),
  });
  const components = useMutation({
    mutationFn: () =>
      api<Schemas["ComponentsStarted"]>("/api/v1/system/updates/components", { method: "POST" }),
    onSuccess: (result) => {
      toast.success(
        result.queued === 0 ? "Every agent and cache is current" : `Updating ${result.queued}`,
      );
      void refresh();
    },
    onError: (error) => toast.error(errorMessage(error, "Could not update components")),
  });

  const data = overview.data;
  if (!data) {
    return (
      <Card title="Updates">
        <p className="text-sm text-muted">
          {restartingFrom ? "The panel is restarting…" : "Loading…"}
        </p>
      </Card>
    );
  }
  const latest = data.last_check?.latest;
  const canApply = !data.apply_blocker && latest && data.last_check?.verified;
  const outdated = data.components.filter(
    (component) => component.agent_outdated || component.cache_outdated,
  );

  return (
    <>
      <Card title="Updates">
        {restartingFrom ? (
          <p className="mb-3 rounded-md border border-line bg-sunken p-3 text-sm">
            Updating from {restartingFrom}. The panel backs itself up, restarts on the new version,
            and rolls back if the new version does not become healthy. This page reconnects on its
            own.
          </p>
        ) : null}
        <dl className="grid gap-2 text-sm sm:grid-cols-[180px_1fr]">
          <dt className="text-muted">Installed</dt>
          <dd>{data.current_version}</dd>
          <dt className="text-muted">Releases from</dt>
          <dd>
            <a
              className="underline"
              href={`https://github.com/${data.repository}/releases`}
              target="_blank"
              rel="noreferrer"
            >
              github.com/{data.repository}
            </a>
          </dd>
          <dt className="text-muted">Last checked</dt>
          <dd>{data.last_check ? formatTime(data.last_check.checked_at) : "never"}</dd>
          <dt className="text-muted">Latest release</dt>
          <dd className="flex flex-wrap items-center gap-2">
            {latest ? (
              <>
                <span className="font-medium">{latest.version}</span>
                <Badge
                  value={data.last_check?.verified ? "signature verified" : "not verified"}
                  tone={data.last_check?.verified ? "ok" : "failed"}
                />
              </>
            ) : data.last_check ? (
              data.last_check.error ? (
                "unknown"
              ) : (
                "you are on the newest release"
              )
            ) : (
              "not checked yet"
            )}
          </dd>
        </dl>
        {data.last_check?.error ? (
          <p className="mt-3 text-sm text-danger">{data.last_check.error}</p>
        ) : null}
        {latest?.notes ? (
          <details className="mt-3 text-sm">
            <summary className="cursor-pointer text-muted">
              Release notes for {latest.version}
            </summary>
            <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap rounded-md bg-sunken p-3 font-sans">
              {latest.notes}
            </pre>
          </details>
        ) : null}
        <div className="mt-4 flex flex-wrap items-center gap-2">
          <Button quiet onClick={() => check.mutate()} disabled={check.isPending}>
            {check.isPending ? "Checking…" : "Check now"}
          </Button>
          <Button
            onClick={() => {
              if (
                window.confirm(
                  `Update the panel to ${latest?.version}? A backup is taken first, and the panel is unavailable for about a minute.`,
                )
              ) {
                apply.mutate();
              }
            }}
            disabled={!canApply || apply.isPending || restartingFrom !== null}
          >
            {latest ? `Update to ${latest.version}` : "Update now"}
          </Button>
          {data.apply_blocker ? (
            <span className="text-sm text-muted">{data.apply_blocker}</span>
          ) : null}
        </div>
        <UpdateSettingsForm data={data} onSaved={refresh} />
      </Card>
      <Card title="Agents and caches">
        <p className="mb-3 text-sm text-muted">
          Agents get the binary bundled with this panel and restart; one that cannot reach the panel
          afterwards puts its previous binary back. Caches are recreated from the release image and
          rolled back if they fail their health check.
        </p>
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead className="text-muted">
              <tr>
                <th className="py-2 pr-3 font-medium">Server</th>
                <th className="py-2 pr-3 font-medium">Agent</th>
                <th className="py-2 pr-3 font-medium">Cache image</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {data.components.map((component) => (
                <tr key={component.server_id}>
                  <td className="py-2 pr-3">
                    {component.server_name}{" "}
                    {component.online ? null : <Badge value="offline" tone="stopped" />}
                  </td>
                  <td className="py-2 pr-3">
                    {component.kind === "local" ? (
                      <span className="text-muted">panel host</span>
                    ) : (
                      <span className="flex items-center gap-2">
                        {component.agent_version ?? "unknown"}
                        {component.agent_outdated ? (
                          <Badge value="outdated" tone="partial" />
                        ) : null}
                      </span>
                    )}
                  </td>
                  <td className="py-2 pr-3">
                    <span className="flex items-center gap-2 break-all font-mono text-xs">
                      {component.cache_image ?? "not installed"}
                      {component.cache_outdated ? <Badge value="outdated" tone="partial" /> : null}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {data.components.length === 0 ? <Empty>No servers yet.</Empty> : null}
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <Button
            quiet
            onClick={() => components.mutate()}
            disabled={components.isPending || outdated.length === 0}
          >
            {outdated.length === 0
              ? "Everything is current"
              : `Update ${outdated.length} server(s)`}
          </Button>
          {data.target_cache_image ? null : (
            <span className="text-sm text-muted">
              No signed release is known for {data.current_version}, so caches are left as they are.
            </span>
          )}
        </div>
      </Card>
      <Card title="Update history">
        <ul className="divide-y divide-line text-sm">
          {data.history.map((entry) => (
            <li key={entry.id} className="py-2">
              <details>
                <summary className="flex cursor-pointer flex-wrap items-center gap-2">
                  <Badge value={entry.status.replace("_", " ")} tone={entry.status} />
                  <span className="font-medium">
                    {entry.component}
                    {entry.server_name ? ` on ${entry.server_name}` : ""}
                  </span>
                  <span className="text-muted">
                    {entry.from_version ?? "?"} → {entry.to_version}
                  </span>
                  <span className="ml-auto text-xs text-muted">{formatTime(entry.started_at)}</span>
                </summary>
                <pre className="mt-2 max-h-72 overflow-auto whitespace-pre-wrap rounded-md bg-sunken p-3 text-xs">
                  {entry.logs || "No output yet."}
                </pre>
              </details>
            </li>
          ))}
        </ul>
        {data.history.length === 0 ? <Empty>No updates yet.</Empty> : null}
      </Card>
    </>
  );
}

function UpdateSettingsForm({ data, onSaved }: { data: Overview; onSaved: () => void }) {
  const t = useT();
  const [channel, setChannel] = useState(data.channel);
  const [autoUpdate, setAutoUpdate] = useState(data.auto_update);
  const [maintenance, setMaintenance] = useState(data.maintenance_window ?? "");
  const save = useMutation({
    mutationFn: () =>
      api<Overview>("/api/v1/system/updates/settings", {
        method: "PUT",
        body: JSON.stringify({
          channel,
          auto_update: autoUpdate,
          maintenance_window: maintenance.trim() || null,
        }),
      }),
    onSuccess: () => {
      toast.success("Update settings saved");
      onSaved();
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save update settings")),
  });
  return (
    <form
      className="mt-5 grid gap-3 border-t border-line pt-4 md:grid-cols-2"
      onSubmit={(event) => {
        event.preventDefault();
        save.mutate();
      }}
    >
      <PickField label="Channel" options={UPDATE_CHANNELS} value={channel} onPick={setChannel} />
      <Field
        label="Maintenance window (UTC)"
        presets={
          <QuickPick
            label={t("Maintenance window presets")}
            options={MAINTENANCE_WINDOWS}
            value={maintenance}
            onPick={setMaintenance}
          />
        }
      >
        <TextInput
          placeholder="02:00-05:00"
          pattern="\d\d:\d\d-\d\d:\d\d"
          value={maintenance}
          onChange={(event) => setMaintenance(event.target.value)}
        />
      </Field>
      <div className="flex flex-wrap items-center gap-3 md:col-span-2">
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={autoUpdate}
            onChange={(event) => setAutoUpdate(event.target.checked)}
          />
          Install verified releases automatically
        </label>
        <Button type="submit" quiet disabled={save.isPending}>
          Save
        </Button>
      </div>
    </form>
  );
}
