import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { lazy, Suspense, useState, type FormEvent } from "react";
import { useParams } from "react-router-dom";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { BackupJobForm, BackupJobs } from "../components/backups";
import { ServiceCachePanel } from "../components/cache";
import { LogViewer } from "../components/logs";
import { DatabasePanel, ServiceLogs } from "../components/DatabasePanel";
import { StackLogs, StackOverview, StackSourceEditor, useStack } from "../components/stacks";
import { MetricsGrid, SERVICE_CHARTS } from "../components/metrics";
import {
  Badge,
  Button,
  Card,
  Empty,
  Field,
  PickField,
  QuickPick,
  Tabs,
  TextInput,
} from "../components/ui";
import { errorMessage } from "../format";
import { useT } from "../i18n";
import { BRANCHES, BUILD_STRATEGIES, ENV_KEYS, IMAGES, PORTS, SSL_MODES } from "../presets";
import type { Schemas } from "../types";

const TerminalPanel = lazy(() =>
  import("../components/terminal").then((module) => ({ default: module.TerminalPanel })),
);

const TABS = [
  ["general", "General"],
  ["source", "Source"],
  ["environment", "Environment"],
  ["domains", "Domains & SSL"],
  ["cache", "Cache"],
  ["deployments", "Deployments"],
  ["metrics", "Metrics"],
  ["logs", "Logs"],
  ["terminal", "Terminal"],
  ["backups", "Backups"],
] as const;

type Tab = (typeof TABS)[number][0];

interface GitSource {
  repo?: string;
  branch?: string;
  strategy?: string;
  provider_id?: string;
}

type Service = Omit<Schemas["ServiceView"], "spec"> & {
  spec: { image?: string; git?: GitSource } & Record<string, unknown>;
};

type Domain = Schemas["DomainView"];

type Deployment = Schemas["DeploymentView"];

type GitProvider = Schemas["GitProviderView"];

type SavedSource = Schemas["SourceView"];

type Registry = Schemas["RegistryView"];

type Match = Schemas["CertificateView"];

export function ServicePage() {
  const t = useT();
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
  const [composeService, setComposeService] = useState("");
  const isStack = service.data?.kind === "compose";
  const stack = useStack(id, isStack);
  const stackServices = isStack ? (stack.data?.services ?? []).map((item) => item.name) : [];
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
          compose_service: isStack ? composeService || undefined : undefined,
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
        ) : current.kind === "compose" ? (
          <StackOverview serviceId={id} />
        ) : (
          <>
            <Card title="Deploy image">
              <form className="grid gap-3 md:grid-cols-2" onSubmit={onDeploy}>
                <Field
                  label="Image"
                  presets={
                    <QuickPick
                      label={t("Image suggestions")}
                      options={IMAGES}
                      value={image}
                      onPick={setImage}
                    />
                  }
                >
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
      {tab === "source" && isStack ? <StackSourceEditor serviceId={id} /> : null}
      {tab === "source" && !isStack ? (
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
              <Field
                label="Branch"
                presets={
                  <QuickPick
                    label={t("Branch presets")}
                    options={BRANCHES}
                    value={branch}
                    onPick={setBranch}
                  />
                }
              >
                <TextInput
                  required
                  value={branch}
                  onChange={(event) => setBranch(event.target.value)}
                />
              </Field>
              <PickField
                label="Build"
                options={BUILD_STRATEGIES}
                value={strategy}
                onPick={setStrategy}
              />
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
              <Field
                label="Port"
                hint={t("The port your app listens on inside the container.")}
                presets={
                  <QuickPick
                    label={t("Port presets")}
                    options={PORTS}
                    value={port}
                    onPick={setPort}
                  />
                }
              >
                <TextInput value={port} onChange={(event) => setPort(event.target.value)} />
              </Field>
              {isStack ? (
                <Field label="Compose service">
                  <select
                    className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                    value={composeService}
                    onChange={(event) => setComposeService(event.target.value)}
                  >
                    <option value="">
                      Main service{stack.data?.main_service ? ` (${stack.data.main_service})` : ""}
                    </option>
                    {stackServices.map((name) => (
                      <option key={name} value={name}>
                        {name}
                      </option>
                    ))}
                  </select>
                </Field>
              ) : null}
              <PickField label="SSL" options={SSL_MODES} value={sslMode} onPick={setSslMode} />
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
                  {domain.compose_service ? ` · → ${domain.compose_service}` : ""}
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
      {tab === "cache" ? <ServiceCachePanel serviceId={id} /> : null}
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
                  {deployment.status === "succeeded" && !isStack ? (
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
      {tab === "metrics" ? (
        <Card title="Metrics">
          <MetricsGrid charts={SERVICE_CHARTS} target={{ service_id: id }} />
        </Card>
      ) : null}
      {tab === "logs" ? (
        <>
          <Card title="Stored logs">
            <LogViewer fixed={{ service_id: id }} sources={["app", "proxy"]} />
          </Card>
          {isStack ? <StackLogs serviceId={id} /> : <ServiceLogs serviceId={id} />}
        </>
      ) : null}
      {tab === "terminal" ? (
        <Card title="Terminal">
          <Suspense fallback={<Empty>Loading terminal…</Empty>}>
            <TerminalPanel serviceId={id} containers={stackServices} />
          </Suspense>
        </Card>
      ) : null}
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
  const t = useT();
  const queryClient = useQueryClient();
  const vars = useQuery({
    queryKey: ["env", serviceId],
    queryFn: () => api<Schemas["EnvVarView"][]>(`/api/v1/services/${serviceId}/env`),
  });
  const [key, setKey] = useState("");
  const [value, setValue] = useState("");
  const [buildSecret, setBuildSecret] = useState(false);
  const suggestedValues = ENV_KEYS.find((item) => item.key === key)?.values ?? [];
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
        <Field
          label="Key"
          presets={
            <QuickPick
              label={t("Common variables")}
              options={ENV_KEYS.map((item) => ({ label: item.key, value: item.key }))}
              value={key}
              onPick={setKey}
            />
          }
        >
          <TextInput required value={key} onChange={(event) => setKey(event.target.value)} />
        </Field>
        <Field
          label="Value"
          presets={
            suggestedValues.length > 0 ? (
              <QuickPick
                label={t("Common values")}
                options={suggestedValues.map((item) => ({ label: item, value: item }))}
                value={value}
                onPick={setValue}
              />
            ) : undefined
          }
        >
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
