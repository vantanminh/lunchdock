import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Button, Card, Field, TextInput } from "../components/ui";

interface Server {
  id: string;
  name: string;
  kind: string;
  status: string;
  public_ip: string | null;
  description: string;
  docker_version: string | null;
  os_info: string | null;
}

interface CreatedServer extends Server {
  join_token: string | null;
  join_command: string | null;
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
  const [created, setCreated] = useState<CreatedServer | null>(null);

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
      setCreated(server);
      setName("");
      toast.success("Server added");
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Could not add server"),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/servers/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Could not remove server"),
  });

  const probe = useMutation({
    mutationFn: (id: string) => api(`/api/v1/servers/${id}/probe`, { method: "POST" }),
    onSuccess: async () => {
      toast.success("Server responded");
      await queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Probe failed"),
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
        {created?.join_command ? (
          <p className="mt-4 break-all rounded-md bg-paper p-3 font-mono text-sm">{created.join_command}</p>
        ) : null}
      </Card>
      <Card title="Fleet">
        <ul className="divide-y divide-line">
          {(servers.data ?? []).map((server) => (
            <li key={server.id} className="flex flex-wrap items-center justify-between gap-3 py-3">
              <div>
                <p className="font-medium">
                  {server.name} <span className="text-muted">{server.kind}</span>
                </p>
                <p className="text-sm text-muted">
                  {server.status}
                  {server.docker_version ? ` · Docker ${server.docker_version}` : ""}
                  {server.public_ip ? ` · ${server.public_ip}` : ""}
                </p>
              </div>
              <div className="flex gap-2">
                {server.kind === "local" ? (
                  <Button quiet onClick={() => probe.mutate(server.id)}>
                    Probe
                  </Button>
                ) : (
                  <Button quiet onClick={() => remove.mutate(server.id)}>
                    Remove
                  </Button>
                )}
              </div>
            </li>
          ))}
        </ul>
      </Card>
    </>
  );
}
