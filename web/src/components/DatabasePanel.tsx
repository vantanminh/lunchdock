import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { useAuth } from "../auth";
import { errorMessage } from "../format";
import { useT } from "../i18n";
import { CPU_LIMITS, MEMORY_LIMITS_MIB } from "../presets";
import type { Schemas } from "../types";
import {
  Button,
  Card,
  CodeBlock,
  DefinitionList,
  Field,
  LogPre,
  QuickPick,
  Segmented,
  TextInput,
} from "./ui";

interface DatabaseSpec {
  engine?: string;
  version?: string;
  database?: string;
  username?: string;
  container?: string;
  volume?: string;
  port?: number;
  public_port?: number | null;
  memory_bytes?: number | null;
  nano_cpus?: number | null;
}

export function DatabasePanel({ service }: { service: Schemas["ServiceView"] }) {
  const t = useT();
  const queryClient = useQueryClient();
  const { user } = useAuth();
  const spec = (service.spec ?? {}) as DatabaseSpec;
  const [connection, setConnection] = useState<Schemas["ConnectionView"] | null>(null);
  const [publicPort, setPublicPort] = useState(spec.public_port ? String(spec.public_port) : "");
  const [memoryMb, setMemoryMb] = useState(
    spec.memory_bytes ? String(Math.round(spec.memory_bytes / 1_048_576)) : "",
  );
  const [cpus, setCpus] = useState(spec.nano_cpus ? String(spec.nano_cpus / 1e9) : "");

  const lifecycle = useMutation({
    mutationFn: (action: "start" | "stop" | "restart") =>
      api<Schemas["LifecycleView"]>(`/api/v1/services/${service.id}/${action}`, { method: "POST" }),
    onSuccess: async (view) => {
      toast.success(t("Database {status}", { status: t(view.status) }));
      await queryClient.invalidateQueries({ queryKey: ["service", service.id] });
      await queryClient.invalidateQueries({ queryKey: ["services"] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Action failed"))),
  });

  const reveal = useMutation({
    mutationFn: () => api<Schemas["ConnectionView"]>(`/api/v1/services/${service.id}/connection`),
    onSuccess: setConnection,
    onError: (error) => toast.error(errorMessage(error, t("Could not load the connection string"))),
  });

  const update = useMutation({
    mutationFn: () =>
      api(`/api/v1/services/${service.id}/database`, {
        method: "PATCH",
        body: JSON.stringify({
          public_port: publicPort ? Number(publicPort) : undefined,
          clear_public_port: !publicPort,
          memory_bytes: memoryMb ? Number(memoryMb) * 1_048_576 : undefined,
          nano_cpus: cpus ? Math.round(Number(cpus) * 1e9) : undefined,
        }),
      }),
    onSuccess: async () => {
      toast.success(t("Saved. Restart the database to apply."));
      await queryClient.invalidateQueries({ queryKey: ["service", service.id] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not save"))),
  });

  return (
    <>
      <Card
        title={t("Database")}
        description={t("Managed container with its own volume and generated credentials.")}
      >
        <DefinitionList
          items={[
            { label: t("Internal host"), value: `${spec.container}:${spec.port}`, mono: true },
            { label: t("Volume"), value: spec.volume || "—", mono: true },
            {
              label: t("Database / user"),
              value: `${spec.database || "—"} / ${spec.username || "—"}`,
            },
            { label: t("Public port"), value: spec.public_port ?? t("internal only") },
          ]}
        />
        <div className="mt-5 flex flex-wrap gap-2 border-t border-line pt-4">
          <Button icon="play" onClick={() => lifecycle.mutate("start")} disabled={lifecycle.isPending}>
            {t("Start")}
          </Button>
          <Button
            variant="secondary"
            icon="stop"
            onClick={() => lifecycle.mutate("stop")}
            disabled={lifecycle.isPending}
          >
            {t("Stop")}
          </Button>
          <Button
            variant="secondary"
            icon="refresh"
            onClick={() => lifecycle.mutate("restart")}
            disabled={lifecycle.isPending}
          >
            {t("Restart")}
          </Button>
          {user?.role === "admin" ? (
            <Button
              variant="ghost"
              icon="lock"
              className="ml-auto"
              disabled={reveal.isPending}
              onClick={() => (connection ? setConnection(null) : reveal.mutate())}
            >
              {connection ? t("Hide connection string") : t("Show connection string")}
            </Button>
          ) : null}
        </div>
        {connection ? (
          <div className="mt-4 space-y-3">
            <div className="space-y-1.5">
              <p className="text-[13px] font-medium text-ink-soft">{t("Internal URL")}</p>
              <CodeBlock copy>{connection.internal_url}</CodeBlock>
            </div>
            {connection.public_url ? (
              <div className="space-y-1.5">
                <p className="text-[13px] font-medium text-ink-soft">{t("Public URL")}</p>
                <CodeBlock copy>{connection.public_url}</CodeBlock>
              </div>
            ) : null}
          </div>
        ) : null}
      </Card>
      <Card
        title={t("Ports and limits")}
        description={t("Changes apply the next time the database restarts.")}
      >
        <form
          className="grid gap-4 md:grid-cols-3"
          onSubmit={(event) => {
            event.preventDefault();
            update.mutate();
          }}
        >
          <Field
            label={t("Public port (empty = internal)")}
            presets={
              <QuickPick
                label={t("Public port presets")}
                options={[
                  { label: t("Internal only"), value: "" },
                  ...(spec.port
                    ? [
                        {
                          label: t("Default ({port})", { port: spec.port }),
                          value: String(spec.port),
                        },
                      ]
                    : []),
                ]}
                value={publicPort}
                onPick={setPublicPort}
              />
            }
          >
            <TextInput value={publicPort} onChange={(event) => setPublicPort(event.target.value)} />
          </Field>
          <Field
            label={t("Memory (MiB)")}
            presets={
              <QuickPick
                label={t("Memory limit presets")}
                options={MEMORY_LIMITS_MIB}
                value={memoryMb}
                onPick={setMemoryMb}
              />
            }
          >
            <TextInput value={memoryMb} onChange={(event) => setMemoryMb(event.target.value)} />
          </Field>
          <Field
            label={t("CPUs")}
            presets={
              <QuickPick
                label={t("CPU limit presets")}
                options={CPU_LIMITS}
                value={cpus}
                onPick={setCpus}
              />
            }
          >
            <TextInput value={cpus} onChange={(event) => setCpus(event.target.value)} />
          </Field>
          <div className="md:col-span-3">
            <Button type="submit" disabled={update.isPending}>
              {t("Save")}
            </Button>
          </div>
        </form>
      </Card>
    </>
  );
}

export function ServiceLogs({ serviceId }: { serviceId: string }) {
  const t = useT();
  const [tail, setTail] = useState("200");
  const logs = useQuery({
    queryKey: ["service-logs", serviceId, tail],
    queryFn: () => api<Schemas["LogsView"]>(`/api/v1/services/${serviceId}/logs?tail=${tail}`),
    refetchInterval: 5000,
    retry: false,
  });
  return (
    <Card
      title={t("Container logs")}
      description={t("Live output of the container. Refreshes every 5 seconds.")}
      actions={
        <Segmented
          label={t("Lines")}
          value={tail}
          onChange={setTail}
          options={[
            ["100", "100"],
            ["200", "200"],
            ["1000", "1000"],
          ]}
        />
      }
    >
      {logs.error ? (
        <p className="text-sm text-muted">{errorMessage(logs.error, t("No logs available"))}</p>
      ) : (
        <LogPre empty={t("No output yet.")}>{logs.data?.logs}</LogPre>
      )}
    </Card>
  );
}
