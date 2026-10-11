import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useParams } from "react-router-dom";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { BackupJobForm, BackupJobs } from "../components/backups";
import { DatabasePanel, ServiceLogs } from "../components/DatabasePanel";
import { Badge, Button, Card, Empty, Field, Tabs, TextInput } from "../components/ui";
import { errorMessage } from "../format";
import type { Schemas } from "../types";

const TABS = [
  ["general", "General"],
  ["source", "Source"],
  ["environment", "Environment"],
  ["domains", "Domains & SSL"],
  ["deployments", "Deployments"],
  ["logs", "Logs"],
  ["backups", "Backups"],
] as const;

type Tab = (typeof TABS)[number][0];

interface GitSource {
  repo?: string;
  branch?: string;
  strategy?: string;
  provider_id?: string;
}

interface Service {
  id: string;
  name: string;
  kind: string;
  status: string;
  server_id: string;
  environment_id: string;
  created_at: string;
  spec: { image?: string; git?: GitSource } & Record<string, unknown>;
}

interface Domain {
  id: string;
  host: string;
  path_prefix: string;
  ssl_mode: string;
  matched_certificate_name: string | null;
  is_preview: boolean;
  published: boolean;
}

interface Deployment {
  id: string;
  status: string;
  trigger: string;
  image_ref: string | null;
  error: string | null;
  logs: string;
  created_at: string;
}

interface GitProvider {
  id: string;
  name: string;
  kind: string;
}

interface SavedSource {
  webhook_secret?: string | null;
  public_key?: string | null;
}

interface Registry {
  id: string;
  name: string;
  url: string;
}

interface Match {
  name?: string;
}

export function ServicePage() {
  const { id = "" } = useParams();
  const queryClient = useQueryClient();
  const service = useQuery({
    queryKey: ["service", id],
    queryFn: () => api<Service>(`/api/v1/services/${id}`),
    enabled: Boolean(id),
  });
  const domains = useQuery({
    queryKey: ["domains", id],
    queryFn: () => api<Domain[]>(`/api/v1/services/${id}/domains`),
    enabled: Boolean(id),
  });
  const deployments = useQuery({
    queryKey: ["deployments", id],
    queryFn: () => api<Deployment[]>(`/api/v1/services/${id}/deployments`),
    enabled: Boolean(id),
    refetchInterval: 4000,
  });
  const registries = useQuery({
    queryKey: ["registries"],
    queryFn: () => api<Registry[]>("/api/v1/registries"),
  });
  const providers = useQuery({
    queryKey: ["git-providers"],
    queryFn: () => api<GitProvider[]>("/api/v1/git-providers"),
  });
  const [image, setImage] = useState("");
  const [registryId, setRegistryId] = useState("");
  const [host, setHost] = useState("");
  const [port, setPort] = useState("80");
  const [sslMode, setSslMode] = useState("letsencrypt");
  const [matchName, setMatchName] = useState<string | null>(null);
  const [providerId, setProviderId] = useState("");
  const [repo, setRepo] = useState("");
  const [branch, setBranch] = useState("main");
  const [strategy, setStrategy] = useState("dockerfile");
  const [tab, setTab] = useState<Tab>("general");
  const [webhookSecret, setWebhookSecret] = useState<string | null>(null);
  const [publicKey, setPublicKey] = useState<string | null>(null);

  const saveSource = useMutation({
    mutationFn: () =>
      api<SavedSource>(`/api/v1/services/${id}/source`, {
        method: "PUT",
        body: JSON.stringify({
          provider_id: providerId,
          repo,
          branch,
          strategy,
        }),
      }),
    onSuccess: async (saved) => {
      setWebhookSecret(saved.webhook_secret ?? null);
      setPublicKey(saved.public_key ?? null);
      toast.success("Source saved. Copy the webhook secret now.");
      await queryClient.invalidateQueries({ queryKey: ["service", id] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not save source"),
  });

  const build = useMutation({
    mutationFn: () => api(`/api/v1/services/${id}/build`, { method: "POST" }),
    onSuccess: async () => {
      toast.success("Build queued");
      await queryClient.invalidateQueries({ queryKey: ["deployments", id] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not queue build"),
  });

  const deploy = useMutation({
    mutationFn: () =>
      api(`/api/v1/services/${id}/deploy`, {
        method: "POST",
        body: JSON.stringify({
          image: image || undefined,
          registry_id: registryId || undefined,
          health_kind: "none",
          health_timeout_secs: 30,
        }),
      }),
    onSuccess: async () => {
      toast.success("Deployment finished");
      await queryClient.invalidateQueries({ queryKey: ["deployments", id] });
      await queryClient.invalidateQueries({ queryKey: ["service", id] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Deploy failed"),
  });

  const addDomain = useMutation({
    mutationFn: () =>
      api(`/api/v1/services/${id}/domains`, {
        method: "POST",
        body: JSON.stringify({
          host,
          target_port: Number(port),
          ssl_mode: sslMode,
        }),
      }),
    onSuccess: async () => {
      toast.success("Domain routed");
      setHost("");
      await queryClient.invalidateQueries({ queryKey: ["domains", id] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not add domain"),
  });

  const preview = useMutation({
    mutationFn: () =>
      api<Domain>(`/api/v1/services/${id}/preview-domain`, {
        method: "POST",
        body: JSON.stringify({ target_port: Number(port) }),
      }),
    onSuccess: async (domain) => {
      toast.success(domain.host);
      await queryClient.invalidateQueries({ queryKey: ["domains", id] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Preview failed"),
  });

  const rollback = useMutation({
    mutationFn: (deploymentId: string) =>
      api(`/api/v1/services/${id}/rollback`, {
        method: "POST",
        body: JSON.stringify({ deployment_id: deploymentId }),
      }),
    onSuccess: async () => {
      toast.success("Rollback finished");
      await queryClient.invalidateQueries({ queryKey: ["deployments", id] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Rollback failed"),
  });

  async function lookUp(nextHost: string) {
    setHost(nextHost);
    if (!nextHost.includes(".")) {
      setMatchName(null);
      return;
    }
    try {
      const match = await api<Match | null>(
        `/api/v1/certificates/match?host=${encodeURIComponent(nextHost)}`,
      );
      setMatchName(match?.name ?? null);
    } catch {
      setMatchName(null);
    }
  }

  function onDeploy(event: FormEvent) {
    event.preventDefault();
    deploy.mutate();
  }

  const current = service.data;

  return (
    <>
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-2xl font-semibold">{current?.name ?? "Service"}</h1>
        {current ? <Badge value={current.status} /> : null}
      </div>
      <p className="text-sm text-muted">
        {current?.kind}
        {current?.spec?.image ? ` · ${current.spec.image}` : ""}
      </p>
      <Tabs tabs={TABS} active={tab} onChange={setTab} />
      {tab === "general" && current ? (
        current.kind === "database" ? (
          <DatabasePanel service={current as unknown as Schemas["ServiceView"]} />
        ) : (
          <>
            <Card title="Deploy image">
              <form className="grid gap-3 md:grid-cols-2" onSubmit={onDeploy}>
                <Field label="Image">
                  <TextInput
                    value={image}
                    placeholder="ghcr.io/org/app:1"
                    onChange={(event) => setImage(event.target.value)}
                  />
                </Field>
                <Field label="Registry">
                  <select
                    className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                    value={registryId}
                    onChange={(event) => setRegistryId(event.target.value)}
                  >
                    <option value="">Match by host</option>
                    {(registries.data ?? []).map((registry) => (
                      <option key={registry.id} value={registry.id}>
                        {registry.name} ({registry.url})
                      </option>
                    ))}
                  </select>
                </Field>
                <Button type="submit" disabled={current?.kind !== "application"}>
                  Pull and deploy
                </Button>
              </form>
            </Card>
          </>
        )
      ) : null}
      {tab === "source" ? (
        <>
          <Card title="Git source">
            <form
              className="grid gap-3 md:grid-cols-2"
              onSubmit={(event) => {
                event.preventDefault();
                saveSource.mutate();
              }}
            >
              <Field label="Provider">
                <select
                  className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                  value={providerId}
                  onChange={(event) => setProviderId(event.target.value)}
                  required
                >
                  <option value="">Choose</option>
                  {(providers.data ?? []).map((provider) => (
                    <option key={provider.id} value={provider.id}>
                      {provider.name} ({provider.kind})
                    </option>
                  ))}
                </select>
              </Field>
              <Field label="Repository">
                <TextInput
                  required
                  value={repo}
                  placeholder="https://github.com/org/app.git"
                  onChange={(event) => setRepo(event.target.value)}
                />
              </Field>
              <Field label="Branch">
                <TextInput
                  required
                  value={branch}
                  onChange={(event) => setBranch(event.target.value)}
                />
              </Field>
              <Field label="Build">
                <select
                  className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                  value={strategy}
                  onChange={(event) => setStrategy(event.target.value)}
                >
                  <option value="dockerfile">Dockerfile</option>
                  <option value="nixpacks">Nixpacks</option>
                  <option value="static">Static site</option>
                </select>
              </Field>
              <div className="flex items-end gap-2">
                <Button type="submit">Save source</Button>
                <Button quiet type="button" onClick={() => build.mutate()}>
                  Build
                </Button>
              </div>
            </form>
            {current?.spec?.git?.repo ? (
              <p className="mt-3 text-sm text-muted">
                Current: {current.spec.git.repo} · {current.spec.git.branch} ·{" "}
                {current.spec.git.strategy}
              </p>
            ) : null}
            {webhookSecret ? <p className="mt-2 text-sm">Webhook secret: {webhookSecret}</p> : null}
            {publicKey ? <p className="mt-2 break-all font-mono text-xs">{publicKey}</p> : null}
          </Card>
        </>
      ) : null}
      {tab === "environment" ? <EnvironmentEditor serviceId={id} /> : null}
      {tab === "domains" ? (
        <>
          <Card title="Domains">
            <form
              className="grid gap-3 md:grid-cols-2"
              onSubmit={(event) => {
                event.preventDefault();
                addDomain.mutate();
              }}
            >
              <Field label="Host">
                <TextInput
                  required
                  value={host}
                  onChange={(event) => void lookUp(event.target.value)}
                />
              </Field>
              <Field label="Port">
                <TextInput value={port} onChange={(event) => setPort(event.target.value)} />
              </Field>
              <Field label="SSL">
                <select
                  className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                  value={sslMode}
                  onChange={(event) => setSslMode(event.target.value)}
                >
                  <option value="letsencrypt">Let's Encrypt</option>
                  <option value="custom">Uploaded certificate</option>
                  <option value="none">None</option>
                </select>
              </Field>
              <div className="flex items-end gap-2">
                <Button type="submit">Add domain</Button>
                <Button quiet type="button" onClick={() => preview.mutate()}>
                  Preview domain
                </Button>
              </div>
            </form>
            <p className="mt-3 text-sm text-muted">
              Certificate match: {matchName ?? "none — Let's Encrypt can still issue one"}
            </p>
            <ul className="mt-4 space-y-2 text-sm">
              {(domains.data ?? []).map((domain) => (
                <li key={domain.id}>
                  {domain.host}
                  {domain.path_prefix !== "/" ? domain.path_prefix : ""} · {domain.ssl_mode}
                  {domain.matched_certificate_name
                    ? ` · cert ${domain.matched_certificate_name}`
                    : ""}
                  {domain.is_preview ? " · preview" : ""}
                  {domain.published ? "" : " · waiting for agent"}
                </li>
              ))}
            </ul>
          </Card>
        </>
      ) : null}
      {tab === "deployments" ? (
        <>
          <Card title="Deployments">
            <ul className="space-y-3 text-sm">
              {(deployments.data ?? []).map((deployment) => (
                <li key={deployment.id} className="rounded-md border border-line p-3">
                  <p>
                    {deployment.status} · {deployment.trigger} ·{" "}
                    {deployment.image_ref ?? "no image"}
                  </p>
                  {deployment.error ? <p className="text-muted">{deployment.error}</p> : null}
                  {deployment.logs ? (
                    <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap text-xs text-muted">
                      {deployment.logs}
                    </pre>
                  ) : null}
                  {deployment.status === "succeeded" ? (
                    <Button quiet className="mt-2" onClick={() => rollback.mutate(deployment.id)}>
                      Roll back
                    </Button>
                  ) : null}
                </li>
              ))}
              {deployments.data?.length === 0 ? (
                <p className="text-muted">No deployments yet.</p>
              ) : null}
            </ul>
          </Card>
        </>
      ) : null}
      {tab === "logs" ? <ServiceLogs serviceId={id} /> : null}
      {tab === "backups" && current ? (
        <>
          <Card title="Backup jobs">
            <BackupJobs serviceId={id} />
          </Card>
          <Card title="New backup job">
            <BackupJobForm
              target={{
                serviceId: id,
                serverId: current.server_id,
                isDatabase: current?.kind === "database",
              }}
            />
          </Card>
        </>
      ) : null}
    </>
  );
}

function EnvironmentEditor({ serviceId }: { serviceId: string }) {
  const queryClient = useQueryClient();
  const vars = useQuery({
    queryKey: ["env", serviceId],
    queryFn: () => api<Schemas["EnvVarView"][]>(`/api/v1/services/${serviceId}/env`),
  });
  const [key, setKey] = useState("");
  const [value, setValue] = useState("");
  const [buildSecret, setBuildSecret] = useState(false);
  const save = useMutation({
    mutationFn: () =>
      api(`/api/v1/services/${serviceId}/env`, {
        method: "PUT",
        body: JSON.stringify({ key, value, is_build_secret: buildSecret }),
      }),
    onSuccess: async () => {
      toast.success(`${key} saved`);
      setKey("");
      setValue("");
      await queryClient.invalidateQueries({ queryKey: ["env", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save the variable")),
  });
  const remove = useMutation({
    mutationFn: (name: string) =>
      api(`/api/v1/services/${serviceId}/env/${encodeURIComponent(name)}`, { method: "DELETE" }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["env", serviceId] }),
    onError: (error) => toast.error(errorMessage(error, "Could not delete the variable")),
  });
  return (
    <Card title="Environment variables">
      <p className="mb-3 text-sm text-muted">
        Values are encrypted and write-only. Saving an existing key replaces its value; redeploy to
        apply.
      </p>
      <form
        className="grid gap-3 md:grid-cols-[1fr_2fr_auto_auto]"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field label="Key">
          <TextInput required value={key} onChange={(event) => setKey(event.target.value)} />
        </Field>
        <Field label="Value">
          <TextInput
            required
            type="password"
            autoComplete="off"
            value={value}
            onChange={(event) => setValue(event.target.value)}
          />
        </Field>
        <label className="flex items-end gap-2 pb-2 text-sm">
          <input
            type="checkbox"
            checked={buildSecret}
            onChange={(event) => setBuildSecret(event.target.checked)}
          />
          Build secret
        </label>
        <div className="flex items-end">
          <Button type="submit">Save</Button>
        </div>
      </form>
      <ul className="mt-4 divide-y divide-line text-sm">
        {(vars.data ?? []).map((item) => (
          <li key={item.key} className="flex items-center gap-2 py-2">
            <span className="font-mono">{item.key}</span>
            {item.is_build_secret ? <span className="text-xs text-muted">build secret</span> : null}
            <button
              type="button"
              className="ml-auto text-accent"
              onClick={() => {
                if (window.confirm(`Delete ${item.key}?`)) {
                  remove.mutate(item.key);
                }
              }}
            >
              Delete
            </button>
          </li>
        ))}
      </ul>
      {vars.data?.length === 0 ? <Empty>No variables.</Empty> : null}
    </Card>
  );
}
