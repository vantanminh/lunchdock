import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Badge, Button, Card, Field, TextInput } from "../components/ui";
import { formatBytes, formatTime } from "../format";
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
  return (
    <div className="mt-4 grid gap-2 rounded-md border border-line p-3">
      <p className="text-sm">
        Run this on <span className="font-medium">{join.server}</span> as root. Docker and systemd
        are required. The token works once
        {join.expiresAt ? ` and expires ${formatTime(join.expiresAt)}` : " and expires in an hour"}.
      </p>
      <p className="break-all rounded-md bg-paper p-3 font-mono text-sm">{join.command}</p>
      <div className="flex gap-2">
        <Button
          quiet
          onClick={() =>
            navigator.clipboard
              .writeText(join.command)
              .then(() => toast.success("Copied"))
              .catch(() => toast.error("Copy failed; select the text instead"))
          }
        >
          Copy
        </Button>
        <Button quiet onClick={onClose}>
          Done
        </Button>
      </div>
    </div>
  );
}

function agentSummary(server: Server): string {
  const parts = [
    server.agent_version ? `agent ${server.agent_version}` : null,
    server.hostname,
    server.os_info,
    server.docker_version ? `Docker ${server.docker_version}` : null,
    server.last_heartbeat_at ? `seen ${formatTime(server.last_heartbeat_at)}` : null,
    server.telemetry_dropped > 0 ? `${server.telemetry_dropped} telemetry items dropped` : null,
  ];
  return parts.filter(Boolean).join(" · ");
}

export function ServersPage() {
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
      toast.success("Server added");
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not add server"),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/servers/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not remove server"),
  });

  const rotate = useMutation({
    mutationFn: (server: Server) =>
      api<Schemas["JoinToken"]>(`/api/v1/servers/${server.id}/join-token`, { method: "POST" }),
    onSuccess: (token, server) => {
      setJoin({ server: server.name, command: token.join_command, expiresAt: token.expires_at });
      window.scrollTo({ top: 0, behavior: "smooth" });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not issue a join token"),
  });

  const probe = useMutation({
    mutationFn: (id: string) => api(`/api/v1/servers/${id}/probe`, { method: "POST" }),
    onSuccess: async () => {
      toast.success("Server responded");
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Probe failed"),
  });

  const prune = useMutation({
    mutationFn: (id: string) =>
      api<Schemas["PruneResult"]>(`/api/v1/servers/${id}/prune`, { method: "POST" }),
    onSuccess: async (result) => {
      toast.success(`Reclaimed ${formatBytes(result.reclaimed_bytes)}`);
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Prune failed"),
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
      toast.error(error instanceof ApiError ? error.message : "Could not update server"),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">Servers</h1>
      <Card title="Add server">
        <form className="grid gap-3 md:grid-cols-4" onSubmit={onSubmit}>
          <Field label="Name">
            <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
          </Field>
          <Field label="Kind">
            <select
              className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
              value={kind}
              onChange={(event) => setKind(event.target.value)}
            >
              <option value="local">local</option>
              <option value="remote">remote</option>
            </select>
          </Field>
          <Field label="Public IP">
            <TextInput value={publicIp} onChange={(event) => setPublicIp(event.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit" disabled={create.isPending}>
              Add
            </Button>
          </div>
        </form>
        {join ? <JoinCommand join={join} onClose={() => setJoin(null)} /> : null}
      </Card>
      <Card title="Fleet">
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
                    ? agentSummary(server) || "Waiting for the agent to enroll"
                    : server.docker_version
                      ? `Docker ${server.docker_version}`
                      : ""}
                  {server.public_ip ? ` · ${server.public_ip}` : ""}
                  {server.last_pruned_at ? ` · pruned ${formatTime(server.last_pruned_at)}` : ""}
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
                  Daily prune
                </label>
                <Button
                  quiet
                  disabled={prune.isPending}
                  title="Remove dangling images and build cache"
                  onClick={() => prune.mutate(server.id)}
                >
                  Prune
                </Button>
                {server.kind === "local" ? (
                  <Button quiet onClick={() => probe.mutate(server.id)}>
                    Probe
                  </Button>
                ) : (
                  <>
                    <Button
                      quiet
                      disabled={rotate.isPending}
                      title="Issue a one-time command that installs or re-enrolls the agent"
                      onClick={() => rotate.mutate(server)}
                    >
                      Join command
                    </Button>
                    <Button
                      quiet
                      onClick={() => {
                        if (window.confirm(`Remove ${server.name}? Its agent is disconnected.`)) {
                          remove.mutate(server.id);
                        }
                      }}
                    >
                      Remove
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
