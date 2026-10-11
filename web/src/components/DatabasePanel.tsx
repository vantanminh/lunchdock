import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { useAuth } from "../auth";
import { errorMessage } from "../format";
import type { Schemas } from "../types";
import { Badge, Button, Card, Field, TextInput } from "./ui";

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
      toast.success(`Database ${view.status}`);
      await queryClient.invalidateQueries({ queryKey: ["service", service.id] });
    },
    onError: (error) => toast.error(errorMessage(error, "Action failed")),
  });

  const reveal = useMutation({
    mutationFn: () => api<Schemas["ConnectionView"]>(`/api/v1/services/${service.id}/connection`),
    onSuccess: setConnection,
    onError: (error) => toast.error(errorMessage(error, "Could not load the connection string")),
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
      toast.success("Saved. Restart the database to apply.");
      await queryClient.invalidateQueries({ queryKey: ["service", service.id] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save")),
  });

  return (
    <>
      <Card title="Database">
        <dl className="grid gap-2 text-sm sm:grid-cols-2">
          <div>
            <dt className="text-muted">Engine</dt>
            <dd>
              {spec.engine} {spec.version}
            </dd>
          </div>
          <div>
            <dt className="text-muted">Status</dt>
            <dd>
              <Badge value={service.status} />
            </dd>
          </div>
          <div>
            <dt className="text-muted">Internal host</dt>
            <dd className="font-mono">
              {spec.container}:{spec.port}
            </dd>
          </div>
          <div>
            <dt className="text-muted">Volume</dt>
            <dd className="font-mono">{spec.volume}</dd>
          </div>
          <div>
            <dt className="text-muted">Database / user</dt>
            <dd>
              {spec.database || "—"} / {spec.username || "—"}
            </dd>
          </div>
          <div>
            <dt className="text-muted">Public port</dt>
            <dd>{spec.public_port ?? "internal only"}</dd>
          </div>
        </dl>
        <div className="mt-4 flex flex-wrap gap-2">
          <Button onClick={() => lifecycle.mutate("start")} disabled={lifecycle.isPending}>
            Start
          </Button>
          <Button quiet onClick={() => lifecycle.mutate("stop")} disabled={lifecycle.isPending}>
            Stop
          </Button>
          <Button quiet onClick={() => lifecycle.mutate("restart")} disabled={lifecycle.isPending}>
            Restart
          </Button>
          {user?.role === "admin" ? (
            <Button quiet onClick={() => (connection ? setConnection(null) : reveal.mutate())}>
              {connection ? "Hide connection string" : "Show connection string"}
            </Button>
          ) : null}
        </div>
        {connection ? (
          <div className="mt-3 space-y-1 rounded-md bg-paper p-3 font-mono text-xs break-all">
            <p>{connection.internal_url}</p>
            {connection.public_url ? <p>{connection.public_url}</p> : null}
          </div>
        ) : null}
      </Card>
      <Card title="Ports and limits">
        <form
          className="grid gap-3 md:grid-cols-4"
          onSubmit={(event) => {
            event.preventDefault();
            update.mutate();
          }}
        >
          <Field label="Public port (empty = internal)">
            <TextInput value={publicPort} onChange={(event) => setPublicPort(event.target.value)} />
          </Field>
          <Field label="Memory (MiB)">
            <TextInput value={memoryMb} onChange={(event) => setMemoryMb(event.target.value)} />
          </Field>
          <Field label="CPUs">
            <TextInput value={cpus} onChange={(event) => setCpus(event.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit">Save</Button>
          </div>
        </form>
      </Card>
    </>
  );
}

export function ServiceLogs({ serviceId }: { serviceId: string }) {
  const [tail, setTail] = useState(200);
  const logs = useQuery({
    queryKey: ["service-logs", serviceId, tail],
    queryFn: () => api<Schemas["LogsView"]>(`/api/v1/services/${serviceId}/logs?tail=${tail}`),
    refetchInterval: 5000,
    retry: false,
  });
  return (
    <Card title="Container logs">
      <div className="mb-3 flex items-center gap-2 text-sm">
        <span className="text-muted">Lines</span>
        {[100, 200, 1000].map((count) => (
          <Button key={count} quiet={tail !== count} onClick={() => setTail(count)}>
            {count}
          </Button>
        ))}
      </div>
      {logs.error ? (
        <p className="text-sm text-muted">{errorMessage(logs.error, "No logs available")}</p>
      ) : (
        <pre className="max-h-[32rem] overflow-auto rounded-md bg-paper p-3 text-xs whitespace-pre-wrap">
          {logs.data?.logs || "No output yet."}
        </pre>
      )}
    </Card>
  );
}
