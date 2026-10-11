import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Button, Card, Field, TextInput } from "../components/ui";
import type { Schemas } from "../types";

type Registry = Schemas["RegistryView"];

export function RegistriesPage() {
  const queryClient = useQueryClient();
  const registries = useQuery({
    queryKey: ["registries"],
    queryFn: () => api<Registry[]>("/api/v1/registries"),
  });
  const [name, setName] = useState("");
  const [url, setUrl] = useState("ghcr.io");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");

  const create = useMutation({
    mutationFn: () =>
      api("/api/v1/registries", {
        method: "POST",
        body: JSON.stringify({ name, url, username, password }),
      }),
    onSuccess: async () => {
      setPassword("");
      toast.success("Registry saved");
      await queryClient.invalidateQueries({ queryKey: ["registries"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Could not save"),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">Registries</h1>
      <Card title="Add credentials">
        <form className="grid gap-3 md:grid-cols-2" onSubmit={onSubmit}>
          <Field label="Name">
            <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
          </Field>
          <Field label="Host">
            <TextInput required value={url} onChange={(event) => setUrl(event.target.value)} />
          </Field>
          <Field label="Username">
            <TextInput value={username} onChange={(event) => setUsername(event.target.value)} />
          </Field>
          <Field label="Password or token">
            <TextInput
              type="password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
            />
          </Field>
          <Button type="submit">Save</Button>
        </form>
      </Card>
      <Card title="Saved">
        <ul className="space-y-2 text-sm">
          {(registries.data ?? []).map((registry) => (
            <li key={registry.id}>
              {registry.name} · {registry.url} · {registry.username ?? "no user"} ·{" "}
              {registry.has_password ? "password stored" : "no password"}
            </li>
          ))}
          {registries.data?.length === 0 ? <p className="text-muted">No registries yet.</p> : null}
        </ul>
      </Card>
    </>
  );
}
