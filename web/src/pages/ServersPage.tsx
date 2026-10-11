import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Badge, Button, Card, Field, PickField, QuickPick, TextInput } from "../components/ui";
import { formatBytes, formatTime } from "../format";
import { useT, type Translate } from "../i18n";
import { SERVER_KINDS, SERVER_NAMES } from "../presets";
import type { Schemas } from "../types";

type Server = Schemas["ServerView"];
type CreatedServer = Schemas["CreatedServer"];

const statusTone: Record<string, string> = {
  online: "ok",
  offline: "failed",
  pending: "queued",
};

interface Join {
  server: string;
  command: string;
  expiresAt?: string;
}

function JoinCommand({ join, onClose }: { join: Join; onClose: () => void }) {
  const t = useT();
  return (
    <div className="mt-4 grid gap-2 rounded-md border border-line p-3">
      <p className="text-sm">
        {t("Run this on {server} as root. Docker and systemd are required.", {
          server: <span className="font-medium">{join.server}</span>,
        })}{" "}
        {join.expiresAt
          ? t("The token works once and expires {time}.", { time: formatTime(join.expiresAt) })
          : t("The token works once and expires in an hour.")}
      </p>
      <p className="break-all rounded-md bg-paper p-3 font-mono text-sm">{join.command}</p>
      <div className="flex gap-2">
        <Button
          quiet
          onClick={() =>
            navigator.clipboard
              .writeText(join.command)
              .then(() => toast.success(t("Copied")))
              .catch(() => toast.error(t("Copy failed; select the text instead")))
          }
        >
          {t("Copy")}
        </Button>
        <Button quiet onClick={onClose}>
          {t("Done")}
        </Button>
      </div>
    </div>
  );
}

function agentSummary(server: Server, t: Translate): string {
  const parts = [
    server.agent_version ? t("agent {version}", { version: server.agent_version }) : null,
    server.hostname,
    server.os_info,
    server.docker_version ? t("Docker {version}", { version: server.docker_version }) : null,
    server.last_heartbeat_at
      ? t("seen {time}", { time: formatTime(server.last_heartbeat_at) })
      : null,
    server.telemetry_dropped > 0
      ? t("{count} telemetry items dropped", { count: server.telemetry_dropped })
      : null,
  ];
  return parts.filter(Boolean).join(" · ");
}

export function ServersPage() {
  const t = useT();
  const queryClient = useQueryClient();
  const servers = useQuery({
    queryKey: ["servers"],
    queryFn: () => api<Server[]>("/api/v1/servers"),
  });
  const [name, setName] = useState("");
  const [kind, setKind] = useState("remote");
  const [publicIp, setPublicIp] = useState("");
  const [join, setJoin] = useState<Join | null>(null);

  const create = useMutation({
    mutationFn: () =>
      api<CreatedServer>("/api/v1/servers", {
        method: "POST",
        body: JSON.stringify({
          name,
          kind,
          public_ip: publicIp || undefined,
        }),
      }),
    onSuccess: async (server) => {
      setJoin(server.join_command ? { server: server.name, command: server.join_command } : null);
      setName("");
      toast.success(t("Server added"));
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not add server")),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/servers/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not remove server")),
  });

  const rotate = useMutation({
    mutationFn: (server: Server) =>
      api<Schemas["JoinToken"]>(`/api/v1/servers/${server.id}/join-token`, { method: "POST" }),
    onSuccess: (token, server) => {
      setJoin({ server: server.name, command: token.join_command, expiresAt: token.expires_at });
      window.scrollTo({ top: 0, behavior: "smooth" });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not issue a join token")),
  });

  const probe = useMutation({
    mutationFn: (id: string) => api(`/api/v1/servers/${id}/probe`, { method: "POST" }),
    onSuccess: async () => {
      toast.success(t("Server responded"));
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : t("Probe failed")),
  });

  const prune = useMutation({
    mutationFn: (id: string) =>
      api<Schemas["PruneResult"]>(`/api/v1/servers/${id}/prune`, { method: "POST" }),
    onSuccess: async (result) => {
      toast.success(t("Reclaimed {size}", { size: formatBytes(result.reclaimed_bytes) }));
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : t("Prune failed")),
  });

  const autoPrune = useMutation({
    mutationFn: (server: Server) =>
      api(`/api/v1/servers/${server.id}`, {
        method: "PATCH",
        body: JSON.stringify({ auto_prune: !server.auto_prune }),
      }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not update server")),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">{t("Servers")}</h1>
      <Card title={t("Add server")}>
        <form className="grid gap-3 md:grid-cols-4" onSubmit={onSubmit}>
          <Field
            label={t("Name")}
            presets={
              <QuickPick
                label={t("Name suggestions")}
                options={SERVER_NAMES}
                value={name}
                onPick={setName}
              />
            }
          >
            <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
          </Field>
          <PickField label={t("Kind")} options={SERVER_KINDS} value={kind} onPick={setKind} />
          <Field label={t("Public IP")}>
            <TextInput value={publicIp} onChange={(event) => setPublicIp(event.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit" disabled={create.isPending}>
              {t("Add")}
            </Button>
          </div>
        </form>
        {join ? <JoinCommand join={join} onClose={() => setJoin(null)} /> : null}
      </Card>
      <Card title={t("Fleet")}>
        <ul className="divide-y divide-line">
          {(servers.data ?? []).map((server) => (
            <li key={server.id} className="flex flex-wrap items-center justify-between gap-3 py-3">
              <div>
                <p className="flex items-center gap-2 font-medium">
                  {server.name}{" "}
                  <span className="text-sm font-normal text-muted">{server.kind}</span>
                  <Badge value={server.status} tone={statusTone[server.status]} />
                </p>
                <p className="text-sm text-muted">
                  {server.kind === "remote"
                    ? agentSummary(server, t) || t("Waiting for the agent to enroll")
                    : server.docker_version
                      ? t("Docker {version}", { version: server.docker_version })
                      : ""}
                  {server.public_ip ? ` · ${server.public_ip}` : ""}
                  {server.last_pruned_at
                    ? ` · ${t("pruned {time}", { time: formatTime(server.last_pruned_at) })}`
                    : ""}
                </p>
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <label className="flex items-center gap-2 text-sm text-muted">
                  <input
                    type="checkbox"
                    checked={server.auto_prune}
                    disabled={autoPrune.isPending}
                    onChange={() => autoPrune.mutate(server)}
                  />
                  {t("Daily prune")}
                </label>
                <Button
                  quiet
                  disabled={prune.isPending}
                  title={t("Remove dangling images and build cache")}
                  onClick={() => prune.mutate(server.id)}
                >
                  {t("Prune")}
                </Button>
                {server.kind === "local" ? (
                  <Button quiet onClick={() => probe.mutate(server.id)}>
                    {t("Probe")}
                  </Button>
                ) : (
                  <>
                    <Button
                      quiet
                      disabled={rotate.isPending}
                      title={t("Issue a one-time command that installs or re-enrolls the agent")}
                      onClick={() => rotate.mutate(server)}
                    >
                      {t("Join command")}
                    </Button>
                    <Button
                      quiet
                      onClick={() => {
                        if (
                          window.confirm(
                            t("Remove {name}? Its agent is disconnected.", { name: server.name }),
                          )
                        ) {
                          remove.mutate(server.id);
                        }
                      }}
                    >
                      {t("Remove")}
                    </Button>
                  </>
                )}
              </div>
            </li>
          ))}
        </ul>
      </Card>
    </>
  );
}
