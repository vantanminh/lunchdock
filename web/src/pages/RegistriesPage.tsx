import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Button, Card, Field, PickField, TextInput } from "../components/ui";
import { useT } from "../i18n";
import { REGISTRIES } from "../presets";
import type { Schemas } from "../types";
import { useConfirmSecret } from "../insecure";

type Registry = Schemas["RegistryView"];

export function RegistriesPage() {
  const t = useT();
  const confirmSecret = useConfirmSecret();
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
      toast.success(t("Registry saved"));
      await queryClient.invalidateQueries({ queryKey: ["registries"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not save")),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (confirmSecret()) {
      create.mutate();
    }
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">{t("Registries")}</h1>
      <Card title={t("Add credentials")}>
        <form className="grid gap-3 md:grid-cols-2" onSubmit={onSubmit}>
          <div className="md:col-span-2">
            <PickField
              label={t("Registry")}
              hint={t(
                "Fills in the name and host. For Docker Hub and GitHub, use an access token as the password.",
              )}
              options={REGISTRIES.map((registry) => ({
                label: registry.label,
                value: registry.url,
              }))}
              value={url}
              onPick={(next) => {
                setUrl(next);
                if (!name) setName(REGISTRIES.find((item) => item.url === next)?.label ?? "");
              }}
            />
          </div>
          <Field label={t("Name")}>
            <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
          </Field>
          <Field label={t("Host")}>
            <TextInput required value={url} onChange={(event) => setUrl(event.target.value)} />
          </Field>
          <Field label={t("Username")}>
            <TextInput value={username} onChange={(event) => setUsername(event.target.value)} />
          </Field>
          <Field label={t("Password or token")}>
            <TextInput
              type="password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
            />
          </Field>
          <Button type="submit">{t("Save")}</Button>
        </form>
      </Card>
      <Card title={t("Saved")}>
        <ul className="space-y-2 text-sm">
          {(registries.data ?? []).map((registry) => (
            <li key={registry.id}>
              {registry.name} · {registry.url} · {registry.username ?? t("no user")} ·{" "}
              {registry.has_password ? t("password stored") : t("no password")}
            </li>
          ))}
          {registries.data?.length === 0 ? (
            <p className="text-muted">{t("No registries yet.")}</p>
          ) : null}
        </ul>
      </Card>
    </>
  );
}
