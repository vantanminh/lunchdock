import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Button, Card, Field, TextArea, TextInput } from "../components/ui";

interface GitProvider {
  id: string;
  name: string;
  kind: string;
}

export function GitProvidersPage() {
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
      toast.success("Git provider saved");
      await queryClient.invalidateQueries({ queryKey: ["git-providers"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Could not save"),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">Git providers</h1>
      <Card title="Add a provider">
        <form className="grid gap-3" onSubmit={onSubmit}>
          <div className="grid gap-3 md:grid-cols-2">
            <Field label="Name">
              <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
            </Field>
            <Field label="Kind">
              <select
                className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                value={kind}
                onChange={(event) => setKind(event.target.value)}
              >
                <option value="github_pat">GitHub personal access token</option>
                <option value="github_app">GitHub App</option>
                <option value="gitlab_pat">GitLab personal access token</option>
                <option value="generic_ssh">SSH deploy key</option>
              </select>
            </Field>
          </div>
          {kind === "github_pat" || kind === "gitlab_pat" ? (
            <Field label="Token">
              <TextInput type="password" value={token} onChange={(event) => setToken(event.target.value)} />
            </Field>
          ) : null}
          {kind === "github_app" ? (
            <>
              <div className="grid gap-3 md:grid-cols-2">
                <Field label="App id">
                  <TextInput value={appId} onChange={(event) => setAppId(event.target.value)} />
                </Field>
                <Field label="Installation id">
                  <TextInput
                    value={installationId}
                    onChange={(event) => setInstallationId(event.target.value)}
                  />
                </Field>
              </div>
              <Field label="Private key PEM">
                <TextArea value={privateKey} onChange={(event) => setPrivateKey(event.target.value)} />
              </Field>
            </>
          ) : null}
          <Button type="submit">Save</Button>
        </form>
      </Card>
      <Card title="Saved">
        <ul className="space-y-2 text-sm">
          {(providers.data ?? []).map((provider) => (
            <li key={provider.id}>
              {provider.name} · {provider.kind}
            </li>
          ))}
          {providers.data?.length === 0 ? <p className="text-muted">No git providers yet.</p> : null}
        </ul>
      </Card>
    </>
  );
}
