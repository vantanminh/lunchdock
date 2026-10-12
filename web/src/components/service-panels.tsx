import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { errorMessage, formatDuration, formatRelative, formatTime } from "../format";
import { providerLabel, providerReady } from "../git";
import { useT } from "../i18n";
import { BRANCHES, BUILD_STRATEGIES, ENV_KEYS, IMAGES, PORTS, SSL_MODES } from "../presets";
import type { Schemas } from "../types";
import { useConfirmSecret } from "../insecure";
import { Icon, type IconName } from "./icons";
import {
  Alert,
  Badge,
  Button,
  Card,
  CodeBlock,
  DataTable,
  DefinitionList,
  Dialog,
  Empty,
  Field,
  LogPre,
  PickField,
  QuickPick,
  Select,
  Td,
  TextArea,
  TextInput,
  Th,
  toneOf,
} from "./ui";

export interface GitSource {
  repo?: string;
  branch?: string;
  strategy?: string;
  provider_id?: string;
}

export type ServiceDetail = Omit<Schemas["ServiceView"], "spec"> & {
  spec: { image?: string; git?: GitSource } & Record<string, unknown>;
};

type Domain = Schemas["DomainView"];
type Deployment = Schemas["DeploymentView"];
type GitProvider = Schemas["GitProviderView"];
type Registry = Schemas["RegistryView"];
type SavedSource = Schemas["SourceView"];
type Match = Schemas["CertificateView"];

/** The current release and a form to deploy another image. Applications only. */
export function DeployImageCard({ service }: { service: ServiceDetail }) {
  const t = useT();
  const queryClient = useQueryClient();
  const registries = useQuery({
    queryKey: ["registries"],
    queryFn: () => api<Registry[]>("/api/v1/registries"),
  });
  const [image, setImage] = useState("");
  const [registryId, setRegistryId] = useState("");

  const deploy = useMutation({
    mutationFn: () =>
      api(`/api/v1/services/${service.id}/deploy`, {
        method: "POST",
        body: JSON.stringify({
          image: image || undefined,
          registry_id: registryId || undefined,
          health_kind: "none",
          health_timeout_secs: 30,
        }),
      }),
    onSuccess: async () => {
      toast.success(t("Deployment finished"));
      await queryClient.invalidateQueries({ queryKey: ["deployments", service.id] });
      await queryClient.invalidateQueries({ queryKey: ["service", service.id] });
      await queryClient.invalidateQueries({ queryKey: ["services"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : t("Deploy failed")),
  });

  const git = service.spec.git;
  return (
    <>
      <Card title={t("Current release")}>
        <DefinitionList
          items={[
            { label: t("Image"), value: service.spec.image ?? "—", mono: true },
            {
              label: t("Source"),
              value: git?.repo ? `${git.repo} · ${git.branch ?? ""}` : t("Container image"),
              mono: Boolean(git?.repo),
            },
          ]}
        />
      </Card>
      <Card
        title={t("Deploy image")}
        description={t("Pull an image and replace the running container.")}
      >
        <form
          className="grid gap-4 md:grid-cols-2"
          onSubmit={(event: FormEvent) => {
            event.preventDefault();
            deploy.mutate();
          }}
        >
          <Field
            label={t("Image")}
            hint={t("Leave empty to redeploy the current image.")}
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
          <Field label={t("Registry")}>
            <Select
              value={registryId}
              onChange={(event) => setRegistryId(event.target.value)}
            >
              <option value="">{t("Match by host")}</option>
              {(registries.data ?? []).map((registry) => (
                <option key={registry.id} value={registry.id}>
                  {registry.name} ({registry.url})
                </option>
              ))}
            </Select>
          </Field>
          <div className="md:col-span-2">
            <Button
              type="submit"
              icon="rocket"
              disabled={service.kind !== "application" || deploy.isPending}
            >
              {deploy.isPending ? t("Deploying…") : t("Pull and deploy")}
            </Button>
          </div>
        </form>
      </Card>
    </>
  );
}

/** Where the code comes from and how it is built. Applications only. */
export function GitSourceCard({ service }: { service: ServiceDetail }) {
  const t = useT();
  const queryClient = useQueryClient();
  const providers = useQuery({
    queryKey: ["git-providers"],
    queryFn: () => api<GitProvider[]>("/api/v1/git-providers"),
  });
  const saved = service.spec.git;
  const [providerId, setProviderId] = useState(saved?.provider_id ?? "");
  const [repo, setRepo] = useState(saved?.repo ?? "");
  const [branch, setBranch] = useState(saved?.branch ?? "main");
  const [strategy, setStrategy] = useState(saved?.strategy ?? "dockerfile");
  const [webhookSecret, setWebhookSecret] = useState<string | null>(null);
  const [publicKey, setPublicKey] = useState<string | null>(null);

  const save = useMutation({
    mutationFn: () =>
      api<SavedSource>(`/api/v1/services/${service.id}/source`, {
        method: "PUT",
        body: JSON.stringify({ provider_id: providerId, repo, branch, strategy }),
      }),
    onSuccess: async (result) => {
      setWebhookSecret(result.webhook_secret ?? null);
      setPublicKey(result.public_key ?? null);
      toast.success(t("Source saved. Copy the webhook secret now."));
      await queryClient.invalidateQueries({ queryKey: ["service", service.id] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not save source")),
  });

  const build = useMutation({
    mutationFn: () => api(`/api/v1/services/${service.id}/build`, { method: "POST" }),
    onSuccess: async () => {
      toast.success(t("Build queued"));
      await queryClient.invalidateQueries({ queryKey: ["deployments", service.id] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not queue build")),
  });

  return (
    <>
      <Card
        title={t("Git source")}
        description={t("Build this service from a repository; pushes can trigger a new build.")}
      >
        <form
          className="grid gap-4 md:grid-cols-2"
          onSubmit={(event: FormEvent) => {
            event.preventDefault();
            save.mutate();
          }}
        >
          <Field label={t("Provider")}>
            <Select
              required
              value={providerId}
              onChange={(event) => setProviderId(event.target.value)}
            >
              <option value="">{t("Choose")}</option>
              {(providers.data ?? []).map((provider) => (
                <option key={provider.id} value={provider.id} disabled={!providerReady(provider)}>
                  {providerLabel(provider, t)}
                </option>
              ))}
            </Select>
          </Field>
          <Field label={t("Repository")}>
            <TextInput
              required
              value={repo}
              placeholder="https://github.com/org/app.git"
              onChange={(event) => setRepo(event.target.value)}
            />
          </Field>
          <Field
            label={t("Branch")}
            presets={
              <QuickPick
                label={t("Branch presets")}
                options={BRANCHES}
                value={branch}
                onPick={setBranch}
              />
            }
          >
            <TextInput required value={branch} onChange={(event) => setBranch(event.target.value)} />
          </Field>
          <PickField
            label={t("Build")}
            options={BUILD_STRATEGIES}
            value={strategy}
            onPick={setStrategy}
          />
          <div className="flex flex-wrap gap-2 md:col-span-2">
            <Button type="submit" disabled={save.isPending}>
              {t("Save source")}
            </Button>
            <Button
              type="button"
              variant="secondary"
              icon="play"
              disabled={!saved?.repo || build.isPending}
              title={saved?.repo ? undefined : t("Save the source first")}
              onClick={() => build.mutate()}
            >
              {t("Build now")}
            </Button>
          </div>
        </form>
      </Card>
      {webhookSecret || publicKey ? (
        <Card title={t("Connect the repository")}>
          <div className="space-y-4">
            <Alert tone="warning" title={t("Shown once")}>
              {t("Copy these now; the secret cannot be displayed again.")}
            </Alert>
            {webhookSecret ? (
              <div className="space-y-1.5">
                <p className="text-[13px] font-medium text-ink-soft">{t("Webhook secret")}</p>
                <CodeBlock copy>{webhookSecret}</CodeBlock>
              </div>
            ) : null}
            {publicKey ? (
              <div className="space-y-1.5">
                <p className="text-[13px] font-medium text-ink-soft">{t("Public key")}</p>
                <CodeBlock copy>{publicKey}</CodeBlock>
              </div>
            ) : null}
          </div>
        </Card>
      ) : null}
    </>
  );
}

function sslLabel(mode: string): string {
  return SSL_MODES.find((option) => option.value === mode)?.label ?? mode;
}

/** Hostnames routed to the service, with a dialog to add one. */
export function DomainsPanel({
  serviceId,
  stackServices,
  mainService,
}: {
  serviceId: string;
  /** Compose service names when the service is a stack; empty otherwise. */
  stackServices: string[];
  mainService?: string | null;
}) {
  const t = useT();
  const [adding, setAdding] = useState(false);
  const domains = useQuery({
    queryKey: ["domains", serviceId],
    queryFn: () => api<Domain[]>(`/api/v1/services/${serviceId}/domains`),
  });
  const list = domains.data ?? [];
  return (
    <>
      <Card
        title={t("Domains")}
        description={t("Hostnames that route traffic to this service.")}
        flush
        actions={
          <Button icon="plus" onClick={() => setAdding(true)}>
            {t("Add domain")}
          </Button>
        }
      >
        {list.length === 0 && !domains.isLoading ? (
          <Empty
            icon="globe"
            title={t("No domains yet")}
            action={
              <Button size="sm" icon="plus" onClick={() => setAdding(true)}>
                {t("Add domain")}
              </Button>
            }
          >
            {t("Route a hostname to this service, or create a preview domain to try it out.")}
          </Empty>
        ) : (
          <DataTable
            minWidth={640}
            head={
              <>
                <Th>{t("Host")}</Th>
                <Th>{t("Routes to")}</Th>
                <Th>{t("SSL")}</Th>
                <Th>{t("Status")}</Th>
              </>
            }
          >
            {list.map((domain) => (
              <tr key={domain.id}>
                <Td>
                  <a
                    href={`${domain.ssl_mode === "none" ? "http" : "https"}://${domain.host}${domain.path_prefix}`}
                    target="_blank"
                    rel="noreferrer"
                    className="inline-flex items-center gap-1.5 font-medium text-ink hover:text-pine"
                  >
                    {domain.host}
                    {domain.path_prefix !== "/" ? (
                      <span className="font-normal text-muted">{domain.path_prefix}</span>
                    ) : null}
                    <Icon name="external" size={13} className="text-faint" />
                  </a>
                  {domain.is_preview ? (
                    <span className="ml-2 align-middle">
                      <Badge value="preview" tone="outline" />
                    </span>
                  ) : null}
                </Td>
                <Td className="font-mono text-[13px] text-ink-soft">
                  :{domain.target_port}
                  {domain.compose_service ? (
                    <span className="text-muted"> → {domain.compose_service}</span>
                  ) : null}
                </Td>
                <Td>
                  <span className="block text-sm text-ink-soft">
                    {t(sslLabel(domain.ssl_mode))}
                  </span>
                  {domain.matched_certificate_name ? (
                    <span className="block text-xs text-muted">
                      {t("cert {name}", { name: domain.matched_certificate_name })}
                    </span>
                  ) : null}
                </Td>
                <Td>
                  <span title={domain.published ? undefined : t("waiting for agent")}>
                    <Badge value={domain.published ? "active" : "pending"} />
                  </span>
                </Td>
              </tr>
            ))}
          </DataTable>
        )}
      </Card>
      {adding ? (
        <AddDomainDialog
          serviceId={serviceId}
          stackServices={stackServices}
          mainService={mainService}
          onClose={() => setAdding(false)}
        />
      ) : null}
    </>
  );
}

function AddDomainDialog({
  serviceId,
  stackServices,
  mainService,
  onClose,
}: {
  serviceId: string;
  stackServices: string[];
  mainService?: string | null;
  onClose: () => void;
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const [host, setHost] = useState("");
  const [port, setPort] = useState("80");
  const [sslMode, setSslMode] = useState("letsencrypt");
  const [composeService, setComposeService] = useState("");
  const [matchName, setMatchName] = useState<string | null>(null);
  const isStack = stackServices.length > 0;

  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: ["domains", serviceId] });
  };

  const add = useMutation({
    mutationFn: () =>
      api(`/api/v1/services/${serviceId}/domains`, {
        method: "POST",
        body: JSON.stringify({
          host,
          target_port: Number(port),
          ssl_mode: sslMode,
          compose_service: isStack ? composeService || undefined : undefined,
        }),
      }),
    onSuccess: async () => {
      toast.success(t("Domain routed"));
      await refresh();
      onClose();
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not add domain")),
  });

  const preview = useMutation({
    mutationFn: () =>
      api<Domain>(`/api/v1/services/${serviceId}/preview-domain`, {
        method: "POST",
        body: JSON.stringify({ target_port: Number(port) }),
      }),
    onSuccess: async (domain) => {
      toast.success(domain.host);
      await refresh();
      onClose();
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Preview failed")),
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

  return (
    <Dialog
      open
      variant="drawer"
      onClose={onClose}
      title={t("Add domain")}
      description={t("Point the hostname's DNS at this server, then route it to a port.")}
    >
      <form
        className="grid gap-5"
        onSubmit={(event: FormEvent) => {
          event.preventDefault();
          add.mutate();
        }}
      >
        <Field
          label={t("Host")}
          hint={t("Certificate match: {name}", {
            name: matchName ?? t("none — Let's Encrypt can still issue one"),
          })}
        >
          <TextInput
            required
            autoFocus
            value={host}
            placeholder="app.example.com"
            onChange={(event) => void lookUp(event.target.value)}
          />
        </Field>
        <Field
          label={t("Port")}
          hint={t("The port your app listens on inside the container.")}
          presets={
            <QuickPick label={t("Port presets")} options={PORTS} value={port} onPick={setPort} />
          }
        >
          <TextInput value={port} onChange={(event) => setPort(event.target.value)} />
        </Field>
        {isStack ? (
          <Field label={t("Compose service")}>
            <Select
              value={composeService}
              onChange={(event) => setComposeService(event.target.value)}
            >
              <option value="">
                {mainService ? t("Main service ({name})", { name: mainService }) : t("Main service")}
              </option>
              {stackServices.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </Select>
          </Field>
        ) : null}
        <PickField label={t("SSL")} options={SSL_MODES} value={sslMode} onPick={setSslMode} />
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line pt-4">
          <Button
            type="button"
            variant="ghost"
            disabled={preview.isPending}
            onClick={() => preview.mutate()}
          >
            {t("Create a preview domain instead")}
          </Button>
          <div className="flex gap-2">
            <Button type="button" variant="secondary" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={add.isPending}>
              {t("Add domain")}
            </Button>
          </div>
        </div>
      </form>
    </Dialog>
  );
}

const statusIcons: Record<string, [IconName, string]> = {
  success: ["check", "border-pine-line bg-pine-soft text-pine"],
  danger: ["alert", "border-danger-line bg-danger-soft text-danger"],
  warning: ["refresh", "border-amber-line bg-amber-soft text-amber"],
  live: ["clock", "animate-live border-line bg-sunken text-ink-soft"],
  neutral: ["info", "border-line bg-sunken text-ink-soft"],
};

/** The last non-empty line of a log: what a running action is doing right now. */
function currentStep(logs: string): string {
  const lines = logs.trimEnd().split("\n");
  return (lines[lines.length - 1] ?? "").trim().slice(0, 160);
}

/** A deployment's log. It opens by itself while the action runs and follows the newest line. */
function DeploymentLog({
  serviceId,
  deployment,
}: {
  serviceId: string;
  deployment: Deployment;
}) {
  const t = useT();
  const running = deployment.status === "running";
  const [open, setOpen] = useState(running);
  return (
    <details
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
      className="group rounded-alert border border-line bg-surface [&_summary::-webkit-details-marker]:hidden"
    >
      <summary className="flex cursor-pointer list-none items-center gap-2 rounded-alert px-3.5 py-2.5 text-sm font-medium text-ink-soft select-none hover:bg-paper">
        <Icon
          name="chevronRight"
          size={14}
          className="text-faint transition-transform group-open:rotate-90"
        />
        {t("Logs")}
        {running ? (
          <span className="ml-1 inline-flex items-center gap-1.5 text-xs font-normal text-muted">
            <span className="animate-live size-1.5 rounded-full bg-current" />
            {t("live")}
          </span>
        ) : null}
      </summary>
      <div className="border-t border-line px-3.5 py-4">
        {deployment.logs_truncated ? (
          <p className="mb-2 text-xs text-muted">
            {t("Showing the end of a long log.")}{" "}
            <a
              className="underline"
              href={`/api/v1/services/${serviceId}/deployments/${deployment.id}/logs`}
              target="_blank"
              rel="noreferrer"
            >
              {t("Open the whole log")}
            </a>
          </p>
        ) : null}
        <LogPre className="max-h-96" empty={t("No log output yet.")} follow={running}>
          {deployment.logs}
        </LogPre>
      </div>
    </details>
  );
}

/** Every build and release of the service, newest first. */
export function DeploymentsPanel({ serviceId, isStack }: { serviceId: string; isStack: boolean }) {
  const t = useT();
  const queryClient = useQueryClient();
  const deployments = useQuery({
    queryKey: ["deployments", serviceId],
    queryFn: () => api<Deployment[]>(`/api/v1/services/${serviceId}/deployments`),
    // Follow a running action closely; an idle service needs little polling.
    refetchInterval: (query) =>
      query.state.data?.some((deployment) => deployment.status === "running") ? 2000 : 4000,
  });
  const cancel = useMutation({
    mutationFn: () => api(`/api/v1/services/${serviceId}/stack/cancel`, { method: "POST" }),
    onSuccess: async () => {
      toast.success(t("Stopping the running action…"));
      await queryClient.invalidateQueries({ queryKey: ["deployments", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not cancel the action"))),
  });
  const rollback = useMutation({
    mutationFn: (deploymentId: string) =>
      api(`/api/v1/services/${serviceId}/rollback`, {
        method: "POST",
        body: JSON.stringify({ deployment_id: deploymentId }),
      }),
    onSuccess: async () => {
      toast.success(t("Rollback finished"));
      await queryClient.invalidateQueries({ queryKey: ["deployments", serviceId] });
      await queryClient.invalidateQueries({ queryKey: ["service", serviceId] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Rollback failed")),
  });

  const list = deployments.data ?? [];
  const currentId = list.find((deployment) => deployment.status === "succeeded")?.id;
  return (
    <Card
      title={t("Deployments")}
      description={t("Every build and release, newest first. Refreshes automatically.")}
      flush
    >
      {list.length === 0 && !deployments.isLoading ? (
        <Empty icon="rocket" title={t("No deployments yet")}>
          {t("Deploy an image or build from Git to create the first release.")}
        </Empty>
      ) : (
        <ol className="divide-y divide-line border-t border-line">
          {list.map((deployment) => {
            const live = deployment.status === "running";
            const [icon, cls] =
              statusIcons[live ? "live" : toneOf(deployment.status)] ?? statusIcons.neutral!;
            const duration =
              deployment.started_at && deployment.finished_at
                ? formatDuration(
                    new Date(deployment.finished_at).getTime() -
                      new Date(deployment.started_at).getTime(),
                  )
                : null;
            return (
              <li key={deployment.id} className="flex gap-3.5 px-5 py-4 sm:px-6">
                <span
                  className={`mt-0.5 inline-flex size-8 shrink-0 items-center justify-center rounded-full border ${cls}`}
                >
                  <Icon name={icon} size={15} />
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge value={deployment.status} />
                    <Badge value={deployment.trigger} tone="outline" />
                    {deployment.id === currentId ? <Badge value="current" tone="success" /> : null}
                    {live && isStack ? (
                      <Button
                        className="ml-auto"
                        variant="danger"
                        size="sm"
                        icon="close"
                        disabled={cancel.isPending}
                        onClick={() => {
                          if (
                            window.confirm(
                              t("Stop the running action? A build in progress is lost."),
                            )
                          ) {
                            cancel.mutate();
                          }
                        }}
                      >
                        {t("Cancel")}
                      </Button>
                    ) : null}
                    {deployment.status === "succeeded" && !isStack && deployment.id !== currentId ? (
                      <Button
                        className="ml-auto"
                        variant="secondary"
                        size="sm"
                        icon="refresh"
                        disabled={rollback.isPending}
                        onClick={() => rollback.mutate(deployment.id)}
                      >
                        {t("Roll back")}
                      </Button>
                    ) : null}
                  </div>
                  <p className="mt-1.5 truncate font-mono text-[13px] text-ink-soft">
                    {live && deployment.logs
                      ? currentStep(deployment.logs)
                      : (deployment.image_ref ?? t("no image"))}
                  </p>
                  <p className="mt-0.5 text-xs text-muted">
                    <time dateTime={deployment.created_at} title={formatTime(deployment.created_at)}>
                      {formatRelative(deployment.created_at)}
                    </time>
                    {duration ? ` · ${t("took {time}", { time: duration })}` : ""}
                  </p>
                  {deployment.error ? (
                    <p className="mt-2 flex items-start gap-1.5 text-sm text-danger">
                      <Icon name="alert" size={15} className="mt-0.5 shrink-0" />
                      {deployment.error}
                    </p>
                  ) : null}
                  {deployment.logs || live ? (
                    <div className="mt-3">
                      <DeploymentLog serviceId={serviceId} deployment={deployment} />
                    </div>
                  ) : null}
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </Card>
  );
}

/** Encrypted, write-only environment variables for the service. */
export function EnvironmentPanel({ serviceId }: { serviceId: string }) {
  const t = useT();
  const confirmSecret = useConfirmSecret();
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
      toast.success(t("{key} saved", { key }));
      setKey("");
      setValue("");
      await queryClient.invalidateQueries({ queryKey: ["env", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not save the variable"))),
  });
  const remove = useMutation({
    mutationFn: (name: string) =>
      api(`/api/v1/services/${serviceId}/env/${encodeURIComponent(name)}`, { method: "DELETE" }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["env", serviceId] }),
    onError: (error) => toast.error(errorMessage(error, t("Could not delete the variable"))),
  });
  const list = vars.data ?? [];
  const [checking, setChecking] = useState<EnvVar | null>(null);
  return (
    <div className="grid gap-6">
      <Card
        title={t("Environment variables")}
        description={t(
          "Values are encrypted and write-only. Saving an existing key replaces its value; redeploy to apply.",
        )}
        flush
      >
        <form
          className="grid gap-4 px-5 pb-5 sm:px-6 md:grid-cols-[1fr_2fr_auto]"
          onSubmit={(event: FormEvent) => {
            event.preventDefault();
            if (confirmSecret()) {
              save.mutate();
            }
          }}
        >
          <Field
            label={t("Key")}
            presets={
              <QuickPick
                label={t("Common variables")}
                options={ENV_KEYS.map((item) => ({ label: item.key, value: item.key }))}
                value={key}
                onPick={setKey}
              />
            }
          >
            <TextInput
              required
              value={key}
              placeholder="DATABASE_URL"
              onChange={(event) => setKey(event.target.value)}
            />
          </Field>
          <Field
            label={t("Value")}
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
          <div className="grid content-start gap-2">
            <span className="hidden text-[13px] font-medium text-transparent select-none md:block">
              {t("Save")}
            </span>
            <Button type="submit" disabled={save.isPending}>
              {t("Save")}
            </Button>
          </div>
          <label className="flex items-center gap-2 text-sm text-ink-soft md:col-span-3">
            <input
              type="checkbox"
              checked={buildSecret}
              onChange={(event) => setBuildSecret(event.target.checked)}
            />
            {t("Build secret")}
            <span className="text-muted">— {t("only available while building the image")}</span>
          </label>
        </form>
        {list.length === 0 && !vars.isLoading ? (
          <div className="border-t border-line">
            <Empty icon="sliders" title={t("No variables yet")}>
              {t("Add the settings and secrets your service reads at startup.")}
            </Empty>
          </div>
        ) : (
          <DataTable
            minWidth={640}
            head={
              <>
                <Th>{t("Key")}</Th>
                <Th>{t("Value")}</Th>
                <Th>{t("Updated")}</Th>
                <Th>{t("Type")}</Th>
                <Th>
                  <span className="sr-only">{t("Actions")}</span>
                </Th>
              </>
            }
          >
            {list.map((item) => (
              <tr key={item.key}>
                <Td className="font-mono text-[13px] font-medium text-ink">{item.key}</Td>
                <Td className="font-mono text-[13px] text-faint" title={t("Write-only")}>
                  •••••••• <span className="font-sans text-xs">{t("{count} characters", { count: item.length })}</span>
                </Td>
                <Td className="text-[13px] text-muted">
                  <time dateTime={item.updated_at} title={formatTime(item.updated_at)}>
                    {formatRelative(item.updated_at)}
                  </time>
                </Td>
                <Td>
                  <Badge
                    value={item.is_build_secret ? "build secret" : "runtime"}
                    tone="outline"
                  />
                </Td>
                <Td className="text-right whitespace-nowrap">
                  <Button
                    variant="ghost"
                    size="sm"
                    icon="check"
                    title={t("Type the value to confirm it was saved correctly")}
                    onClick={() => setChecking(item)}
                  >
                    {t("Check value")}
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    icon="trash"
                    className="text-danger hover:bg-danger-soft hover:text-danger"
                    aria-label={t("Delete {key}", { key: item.key })}
                    title={t("Delete")}
                    onClick={() => {
                      if (window.confirm(t("Delete {key}?", { key: item.key }))) {
                        remove.mutate(item.key);
                      }
                    }}
                  />
                </Td>
              </tr>
            ))}
          </DataTable>
        )}
      </Card>
      <ImportEnvCard serviceId={serviceId} />
      {checking ? (
        <CheckValueDialog serviceId={serviceId} item={checking} onClose={() => setChecking(null)} />
      ) : null}
    </div>
  );
}

type EnvVar = Schemas["EnvVarView"];
type ImportPreview = Schemas["ImportEnvResult"];

/** Paste or upload a `.env` file, see what would change, then save every key at once. */
function ImportEnvCard({ serviceId }: { serviceId: string }) {
  const t = useT();
  const confirmSecret = useConfirmSecret();
  const queryClient = useQueryClient();
  const [text, setText] = useState("");
  const [buildSecret, setBuildSecret] = useState(false);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const run = useMutation({
    mutationFn: (dryRun: boolean) =>
      api<ImportPreview>(`/api/v1/services/${serviceId}/env/import`, {
        method: "POST",
        body: JSON.stringify({ text, is_build_secret: buildSecret, dry_run: dryRun }),
      }),
    onSuccess: async (result, dryRun) => {
      setPreview(result);
      if (!dryRun) {
        const count = result.entries.filter((entry) => entry.status !== "unchanged").length;
        toast.success(t("{count} variables saved", { count }));
        setText("");
        setPreview(null);
        await queryClient.invalidateQueries({ queryKey: ["env", serviceId] });
        await queryClient.invalidateQueries({ queryKey: ["stack", serviceId] });
      }
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not import the variables"))),
  });
  const changes = preview?.entries.filter((entry) => entry.status !== "unchanged").length ?? 0;
  return (
    <Card
      title={t("Import .env")}
      description={t(
        "Paste the contents of a .env file or choose one. Nothing is saved until you press Apply.",
      )}
    >
      <div className="grid gap-4">
        <TextArea
          aria-label={t("Contents of a .env file")}
          className="min-h-40"
          placeholder={"DATABASE_URL=postgres://user:pass@db/app\nPORT=3000"}
          autoComplete="off"
          spellCheck={false}
          value={text}
          onChange={(event) => {
            setText(event.target.value);
            setPreview(null);
          }}
        />
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
          <label className="inline-flex cursor-pointer items-center gap-2 text-sm font-medium text-ink-soft underline-offset-2 hover:text-ink hover:underline">
            <Icon name="download" size={15} />
            {t("Choose a file")}
            <input
              type="file"
              className="sr-only"
              onChange={async (event) => {
                const file = event.target.files?.[0];
                event.target.value = "";
                if (file) {
                  setText(await file.text());
                  setPreview(null);
                }
              }}
            />
          </label>
          <label className="flex items-center gap-2 text-sm text-ink-soft">
            <input
              type="checkbox"
              checked={buildSecret}
              onChange={(event) => {
                setBuildSecret(event.target.checked);
                setPreview(null);
              }}
            />
            {t("Build secret")}
          </label>
          <div className="ml-auto flex gap-2">
            <Button
              variant="secondary"
              disabled={!text.trim() || run.isPending}
              onClick={() => run.mutate(true)}
            >
              {t("Preview")}
            </Button>
            <Button
              disabled={!preview || preview.errors.length > 0 || changes === 0 || run.isPending}
              onClick={() => {
                if (confirmSecret()) {
                  run.mutate(false);
                }
              }}
            >
              {t("Apply")}
            </Button>
          </div>
        </div>
        {preview ? (
          <div className="grid gap-3">
            {preview.errors.length > 0 ? (
              <Alert tone="error" title={t("Fix these lines first")}>
                <ul className="list-disc pl-5">
                  {preview.errors.map((error) => (
                    <li key={`${error.line}-${error.message}`}>
                      {t("Line {line}: {message}", { line: error.line, message: error.message })}
                    </li>
                  ))}
                </ul>
              </Alert>
            ) : null}
            {preview.entries.length > 0 ? (
              <ul className="divide-y divide-line rounded-control border border-line text-sm">
                {preview.entries.map((entry) => (
                  <li key={entry.key} className="flex items-center justify-between gap-3 px-3 py-2">
                    <span className="min-w-0 truncate font-mono text-[13px] text-ink">
                      {entry.key}
                    </span>
                    <span className="flex shrink-0 items-center gap-3 text-xs text-muted">
                      {t("{count} characters", { count: entry.length })}
                      <Badge
                        value={entry.status}
                        tone={entry.status === "unchanged" ? "outline" : "live"}
                      />
                    </span>
                  </li>
                ))}
              </ul>
            ) : null}
            {preview.errors.length === 0 ? (
              <p className="text-sm text-muted">
                {changes === 0
                  ? t("Every key is already saved with this value.")
                  : t("{count} variables will be saved. Values are never shown again.", {
                      count: changes,
                    })}
              </p>
            ) : null}
          </div>
        ) : null}
      </div>
    </Card>
  );
}

/** Type a value and learn whether it equals the saved one; the saved value is never sent back. */
function CheckValueDialog({
  serviceId,
  item,
  onClose,
}: {
  serviceId: string;
  item: EnvVar;
  onClose: () => void;
}) {
  const t = useT();
  const confirmSecret = useConfirmSecret();
  const [value, setValue] = useState("");
  const verify = useMutation({
    mutationFn: () =>
      api<Schemas["VerifyEnvResult"]>(
        `/api/v1/services/${serviceId}/env/${encodeURIComponent(item.key)}/verify`,
        {
          method: "POST",
          body: JSON.stringify({ value, is_build_secret: item.is_build_secret }),
        },
      ),
    onError: (error) => toast.error(errorMessage(error, t("Could not check the value"))),
  });
  return (
    <Dialog
      open
      onClose={onClose}
      title={t("Check {key}", { key: item.key })}
      description={t(
        "The saved value is never shown. Type what you expect it to be and the panel says whether it matches.",
      )}
    >
      <form
        className="grid gap-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault();
          if (confirmSecret()) {
            verify.mutate();
          }
        }}
      >
        <Field label={t("Value")}>
          <TextInput
            required
            type="password"
            autoComplete="off"
            autoFocus
            value={value}
            onChange={(event) => {
              setValue(event.target.value);
              verify.reset();
            }}
          />
        </Field>
        {verify.data ? (
          verify.data.matches ? (
            <Alert tone="success">{t("It matches the saved value.")}</Alert>
          ) : (
            <Alert tone="error">{t("It does not match the saved value.")}</Alert>
          )
        ) : null}
        <div className="flex justify-end gap-2">
          <Button variant="secondary" onClick={onClose}>
            {t("Close")}
          </Button>
          <Button type="submit" disabled={verify.isPending}>
            {t("Check")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
