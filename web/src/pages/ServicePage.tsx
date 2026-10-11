import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useParams } from "react-router-dom";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Button, Card, Field, TextInput } from "../components/ui";

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
  spec: { image?: string; git?: GitSource };
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
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Could not save source"),
  });

  const build = useMutation({
    mutationFn: () => api(`/api/v1/services/${id}/build`, { method: "POST" }),
    onSuccess: async () => {
      toast.success("Build queued");
      await queryClient.invalidateQueries({ queryKey: ["deployments", id] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Could not queue build"),
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
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Could not add domain"),
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
      <h1 className="text-2xl font-semibold">{current?.name ?? "Service"}</h1>
      <p className="text-sm text-muted">
        {current?.kind} · {current?.status}
        {current?.spec?.image ? ` · ${current.spec.image}` : ""}
      </p>
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
            <TextInput required value={branch} onChange={(event) => setBranch(event.target.value)} />
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
            Current: {current.spec.git.repo} · {current.spec.git.branch} · {current.spec.git.strategy}
          </p>
        ) : null}
        {webhookSecret ? (
          <p className="mt-2 text-sm">Webhook secret: {webhookSecret}</p>
        ) : null}
        {publicKey ? <p className="mt-2 break-all font-mono text-xs">{publicKey}</p> : null}
      </Card>
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
      <Card title="Domains">
        <form
          className="grid gap-3 md:grid-cols-2"
          onSubmit={(event) => {
            event.preventDefault();
            addDomain.mutate();
          }}
        >
          <Field label="Host">
            <TextInput required value={host} onChange={(event) => void lookUp(event.target.value)} />
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
              {domain.matched_certificate_name ? ` · cert ${domain.matched_certificate_name}` : ""}
              {domain.is_preview ? " · preview" : ""}
              {domain.published ? "" : " · waiting for agent"}
            </li>
          ))}
        </ul>
      </Card>
      <Card title="Deployments">
        <ul className="space-y-3 text-sm">
          {(deployments.data ?? []).map((deployment) => (
            <li key={deployment.id} className="rounded-md border border-line p-3">
              <p>
                {deployment.status} · {deployment.trigger} · {deployment.image_ref ?? "no image"}
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
          {deployments.data?.length === 0 ? <p className="text-muted">No deployments yet.</p> : null}
        </ul>
      </Card>
    </>
  );
}
