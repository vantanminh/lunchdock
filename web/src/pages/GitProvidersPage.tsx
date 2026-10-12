import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Button, Card, Field, PickField, TextArea, TextInput } from "../components/ui";
import { msg, useT } from "../i18n";
import type { Schemas } from "../types";

type GitProvider = Schemas["GitProviderView"];

const KINDS = [
  { value: "github_pat", label: msg("GitHub token"), name: "GitHub" },
  { value: "github_app", label: msg("GitHub App"), name: "GitHub App" },
  { value: "gitlab_pat", label: msg("GitLab token"), name: "GitLab" },
  { value: "generic_ssh", label: msg("SSH deploy key"), name: "Deploy key" },
];

export function GitProvidersPage() {
  const t = useT();
  const queryClient = useQueryClient();
  const providers = useQuery({
    queryKey: ["git-providers"],
    queryFn: () => api<GitProvider[]>("/api/v1/git-providers"),
  });
  const [name, setName] = useState("");
  const [kind, setKind] = useState("github_pat");
  const [token, setToken] = useState("");
  const [appId, setAppId] = useState("");
  const [installationId, setInstallationId] = useState("");
  const [privateKey, setPrivateKey] = useState("");

  const create = useMutation({
    mutationFn: () =>
      api("/api/v1/git-providers", {
        method: "POST",
        body: JSON.stringify({
          name,
          kind,
          token: token || undefined,
          app_id: appId || undefined,
          installation_id: installationId || undefined,
          private_key_pem: privateKey || undefined,
        }),
      }),
    onSuccess: async () => {
      setToken("");
      setPrivateKey("");
      toast.success(t("Git provider saved"));
      await queryClient.invalidateQueries({ queryKey: ["git-providers"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not save")),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">{t("Git providers")}</h1>
      <Card title={t("Add a provider")}>
        <form className="grid gap-3" onSubmit={onSubmit}>
          <div className="grid gap-3 md:grid-cols-2">
            <Field label={t("Name")}>
              <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
            </Field>
            <PickField
              label={t("Kind")}
              hint={
                kind === "github_pat"
                  ? t("A personal access token with read access to the repositories.")
                  : kind === "generic_ssh"
                    ? t(
                        "Each service gets its own key; its public key is shown after you save the service source.",
                      )
                    : undefined
              }
              options={KINDS}
              value={kind}
              onPick={(next) => {
                const previous = KINDS.find((item) => item.value === kind);
                setKind(next);
                if (!name || name === previous?.name) {
                  setName(KINDS.find((item) => item.value === next)?.name ?? "");
                }
              }}
            />
          </div>
          {kind === "github_pat" || kind === "gitlab_pat" ? (
            <Field label={t("Token")}>
              <TextInput
                type="password"
                value={token}
                onChange={(event) => setToken(event.target.value)}
              />
            </Field>
          ) : null}
          {kind === "github_app" ? (
            <>
              <div className="grid gap-3 md:grid-cols-2">
                <Field label={t("App id")}>
                  <TextInput value={appId} onChange={(event) => setAppId(event.target.value)} />
                </Field>
                <Field label={t("Installation id")}>
                  <TextInput
                    value={installationId}
                    onChange={(event) => setInstallationId(event.target.value)}
                  />
                </Field>
              </div>
              <Field label={t("Private key PEM")}>
                <TextArea
                  value={privateKey}
                  onChange={(event) => setPrivateKey(event.target.value)}
                />
              </Field>
            </>
          ) : null}
          <Button type="submit">{t("Save")}</Button>
        </form>
      </Card>
      <Card title={t("Saved")}>
        <ul className="space-y-2 text-sm">
          {(providers.data ?? []).map((provider) => (
            <li key={provider.id}>
              {provider.name} · {provider.kind}
            </li>
          ))}
          {providers.data?.length === 0 ? (
            <p className="text-muted">{t("No git providers yet.")}</p>
          ) : null}
        </ul>
      </Card>
    </>
  );
}
