import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { toast } from "sonner";
import { api } from "../api";
import { errorMessage } from "../format";
import { providerLabel, providerReady } from "../git";
import { useT } from "../i18n";
import {
  BRANCHES,
  COMPOSE_FILES,
  COMPOSE_STARTERS,
  MAIN_SERVICES,
  STACK_SOURCES,
} from "../presets";
import type { Schemas } from "../types";
import {
  Alert,
  Badge,
  Button,
  Card,
  CodeBlock,
  DataTable,
  DefinitionList,
  Disclosure,
  Empty,
  Field,
  LogPre,
  PickField,
  QuickPick,
  Segmented,
  Select,
  Td,
  TextArea,
  TextInput,
  Th,
} from "./ui";

type StackView = Schemas["StackView"];
type StackGit = Schemas["StackGit"];

type GitProvider = Schemas["GitProviderView"];

export const COMPOSE_EXAMPLE = `services:
  web:
    image: nginx:1.27-alpine
    environment:
      GREETING: \${GREETING}
`;

export function useStack(serviceId: string, enabled = true) {
  return useQuery({
    queryKey: ["stack", serviceId],
    queryFn: () => api<StackView>(`/api/v1/services/${serviceId}/stack`),
    enabled: enabled && Boolean(serviceId),
    refetchInterval: (query) => (query.state.data?.service.status === "deploying" ? 2000 : 10000),
  });
}

export function StackOverview({ serviceId }: { serviceId: string }) {
  const t = useT();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const stack = useStack(serviceId);
  const action = useMutation({
    mutationFn: (name: "up" | "rebuild" | "update" | "down" | "restart" | "pull") =>
      api<Schemas["DeploymentView"]>(`/api/v1/services/${serviceId}/stack/${name}`, {
        method: "POST",
      }),
    onSuccess: async (_deployment, name) => {
      toast.success(t("docker compose {name} started; follow it under Deployments", { name }));
      await queryClient.invalidateQueries({ queryKey: ["stack", serviceId] });
      await queryClient.invalidateQueries({ queryKey: ["deployments", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Stack action failed"))),
  });
  const refresh = useMutation({
    mutationFn: () =>
      api<StackView>(`/api/v1/services/${serviceId}/stack/refresh`, { method: "POST" }),
    onSuccess: async (next) => {
      queryClient.setQueryData(["stack", serviceId], next);
      toast.success(
        next.missing_variables.length > 0
          ? t("Read from Git. {count} variables still need a value.", {
              count: next.missing_variables.length,
            })
          : t("Read from Git. Nothing is missing."),
      );
      await queryClient.invalidateQueries({ queryKey: ["stack", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not read the compose file from Git"))),
  });
  const cancel = useMutation({
    mutationFn: () =>
      api(`/api/v1/services/${serviceId}/stack/cancel`, { method: "POST" }),
    onSuccess: async () => {
      toast.success(t("Stopping the running action…"));
      await queryClient.invalidateQueries({ queryKey: ["stack", serviceId] });
      await queryClient.invalidateQueries({ queryKey: ["deployments", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not cancel the action"))),
  });
  const remove = useMutation({
    mutationFn: (volumes: boolean) =>
      api(`/api/v1/services/${serviceId}/stack?volumes=${volumes}`, { method: "DELETE" }),
    onSuccess: () => {
      toast.success(t("Stack deleted"));
      navigate("/projects");
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not delete the stack"))),
  });
  const view = stack.data;
  if (stack.error) {
    return <Empty>{errorMessage(stack.error, t("Could not load the stack"))}</Empty>;
  }
  if (!view) {
    return <Empty>{t("Loading stack…")}</Empty>;
  }
  const running = view.service.status === "deploying";
  const busy = running || action.isPending;
  const builds = view.services.some((service) => service.build);
  const pulls = view.services.some((service) => service.image);
  const unread = view.source === "git" && !view.compose_yaml;
  return (
    <>
      {unread ? (
        <Alert
          tone="info"
          title={t("The compose file has not been read from Git yet")}
          action={
            <Button
              size="sm"
              icon="refresh"
              disabled={busy || refresh.isPending}
              onClick={() => refresh.mutate()}
            >
              {t("Refresh from Git")}
            </Button>
          }
        >
          {t(
            "Read it now to see the variables it needs, the services and the generated file before the first deploy.",
          )}
        </Alert>
      ) : null}
      {view.missing_variables.length > 0 ? (
        <Alert tone="warning">
          {t("Not set under Environment: {names}", { names: view.missing_variables.join(", ") })}
        </Alert>
      ) : null}
      {view.warnings.length > 0 ? (
        <Alert tone="info">
          <ul className="list-disc space-y-1 pl-4">
            {view.warnings.map((warning) => (
              <li key={warning}>{warning}</li>
            ))}
          </ul>
        </Alert>
      ) : null}
      <Card
        title={t("Stack")}
        description={t("A group of containers started together with docker compose.")}
      >
        <DefinitionList
          items={[
            { label: t("Project"), value: view.project, mono: true },
            { label: t("Source"), value: view.source === "git" ? t("Git") : t("pasted YAML") },
            ...(view.template
              ? [{ label: t("Template"), value: <Badge value={view.template} tone="outline" /> }]
              : []),
          ]}
        />
        <div className="mt-5 flex flex-wrap gap-2 border-t border-line pt-4">
          <Button icon="play" disabled={busy} onClick={() => action.mutate("up")}>
            {view.build_on_deploy ? t("Build and deploy") : t("Deploy (up)")}
          </Button>
          {builds && !view.build_on_deploy ? (
            <Button
              variant="secondary"
              icon="zap"
              disabled={busy}
              title={t("Rebuild the images that have a build section, then deploy")}
              onClick={() => action.mutate("rebuild")}
            >
              {t("Rebuild and deploy")}
            </Button>
          ) : null}
          {view.source === "git" && !unread ? (
            <Button
              variant="secondary"
              icon="refresh"
              disabled={busy || refresh.isPending}
              title={t("Read the compose file from the repository again without deploying")}
              onClick={() => refresh.mutate()}
            >
              {t("Refresh from Git")}
            </Button>
          ) : null}
          {pulls ? (
            <Button
              variant="secondary"
              icon="download"
              disabled={busy}
              title={t("Fetch newer versions of every image, then deploy")}
              onClick={() => action.mutate("update")}
            >
              {t("Pull and deploy")}
            </Button>
          ) : null}
          <Button
            variant="secondary"
            icon="refresh"
            disabled={busy}
            onClick={() => action.mutate("restart")}
          >
            {t("Restart")}
          </Button>
          <Button
            variant="secondary"
            icon="download"
            disabled={busy}
            title={t("Download images without restarting anything")}
            onClick={() => action.mutate("pull")}
          >
            {t("Pull images")}
          </Button>
          <Button
            variant="secondary"
            icon="stop"
            disabled={busy}
            onClick={() => action.mutate("down")}
          >
            {t("Stop (down)")}
          </Button>
          {running ? (
            <Button
              variant="danger"
              icon="close"
              disabled={cancel.isPending}
              onClick={() => {
                if (window.confirm(t("Stop the running action? A build in progress is lost."))) {
                  cancel.mutate();
                }
              }}
            >
              {t("Cancel")}
            </Button>
          ) : null}
          <Button
            variant="danger"
            icon="trash"
            className="ml-auto"
            disabled={busy}
            onClick={() => {
              if (
                !window.confirm(
                  t("Delete {name}? Containers are removed.", { name: view.service.name }),
                )
              ) {
                return;
              }
              const volumes = window.confirm(
                t(
                  "Also delete the stack's named volumes? This erases its data. Cancel keeps them.",
                ),
              );
              remove.mutate(volumes);
            }}
          >
            {t("Delete stack")}
          </Button>
        </div>
      </Card>
      <Card title={t("Services")} flush>
        {view.services.length === 0 ? (
          <Empty>
            {t("The compose file is read from Git on the first deploy, or when you press Refresh from Git.")}
          </Empty>
        ) : (
          <DataTable
            minWidth={560}
            head={
              <>
                <Th>{t("Service")}</Th>
                <Th>{t("Image")}</Th>
                <Th>{t("Container")}</Th>
                <Th>{t("State")}</Th>
              </>
            }
          >
            {view.services.map((service) => (
              <tr key={service.name}>
                <Td className="font-medium text-ink">
                  {service.name}
                  {service.routed ? (
                    <span className="ml-2 align-middle">
                      <Badge value="routed" tone="outline" />
                    </span>
                  ) : null}
                </Td>
                <Td className="font-mono text-[13px] text-ink-soft">
                  {service.image ?? t("build")}
                  {service.build && service.image ? (
                    <span className="ml-2 align-middle font-sans">
                      <Badge value="builds from source" tone="outline" />
                    </span>
                  ) : null}
                </Td>
                <Td className="font-mono text-[13px] text-muted">{service.container}</Td>
                <Td>
                  <Badge
                    value={
                      service.running == null
                        ? "unreachable"
                        : service.running
                          ? "running"
                          : "stopped"
                    }
                  />
                </Td>
              </tr>
            ))}
          </DataTable>
        )}
      </Card>
      <Disclosure title={t("Generated compose file")}>
        <p className="mb-3 text-sm text-muted">
          {view.compose_path
            ? t(
                "What Rudder runs from {path}: your file with the project name, rudder labels, and the rudder-proxy network for routed services.",
                { path: view.compose_path },
              )
            : t(
                "What Rudder runs: your file with the project name, rudder labels, and the rudder-proxy network for routed services.",
              )}
        </p>
        <CodeBlock copy className="max-h-[28rem]">
          {view.generated_yaml || t("Not generated yet.")}
        </CodeBlock>
      </Disclosure>
    </>
  );
}

export function StackSourceEditor({ serviceId }: { serviceId: string }) {
  const t = useT();
  const stack = useStack(serviceId);
  const providers = useQuery({
    queryKey: ["git-providers"],
    queryFn: () => api<GitProvider[]>("/api/v1/git-providers"),
  });
  if (!stack.data) {
    return (
      <Empty>{stack.error ? errorMessage(stack.error, t("Could not load")) : t("Loading…")}</Empty>
    );
  }
  return (
    <SourceForm
      key={stack.data.service.id}
      serviceId={serviceId}
      initial={stack.data}
      providers={providers.data ?? []}
    />
  );
}

/** Starter compose files; picking one fills the editor after confirming it will replace the text. */
function YamlStarters({ yaml, onPick }: { yaml: string; onPick: (yaml: string) => void }) {
  const t = useT();
  return (
    <QuickPick
      label={t("Starter files")}
      options={COMPOSE_STARTERS.map((starter) => ({ label: starter.label, value: starter.yaml }))}
      value={yaml}
      onPick={(next) => {
        if (!yaml.trim() || window.confirm(t("Replace the current YAML with this starter?"))) {
          onPick(next);
        }
      }}
    />
  );
}

function SourceForm({
  serviceId,
  initial,
  providers,
}: {
  serviceId: string;
  initial: StackView;
  providers: GitProvider[];
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const [source, setSource] = useState(initial.source || "inline");
  const [yaml, setYaml] = useState(initial.compose_yaml);
  const [git, setGit] = useState<StackGit>(
    initial.git ?? {
      provider_id: "",
      repo: "",
      branch: "main",
      compose_file: "docker-compose.yml",
    },
  );
  const [mainService, setMainService] = useState(initial.main_service ?? "");
  const [buildOnDeploy, setBuildOnDeploy] = useState(initial.build_on_deploy);
  const [deployOnPush, setDeployOnPush] = useState(initial.deploy_on_push);
  const hasBuilds = initial.services.some((service) => service.build);
  const save = useMutation({
    mutationFn: (deploy: boolean) =>
      api<StackView>(`/api/v1/services/${serviceId}/stack`, {
        method: "PUT",
        body: JSON.stringify({
          source,
          compose_yaml: source === "inline" ? yaml : undefined,
          git: source === "git" ? git : undefined,
          main_service: mainService || undefined,
          build_on_deploy: buildOnDeploy,
          deploy_on_push: source === "git" ? deployOnPush : false,
        }),
      }).then(async (view) => {
        if (deploy) {
          await api(`/api/v1/services/${serviceId}/stack/up`, { method: "POST" });
        }
        return view;
      }),
    onSuccess: async (_view, deploy) => {
      toast.success(deploy ? t("Saved; docker compose up started") : t("Compose source saved"));
      await queryClient.invalidateQueries({ queryKey: ["stack", serviceId] });
      await queryClient.invalidateQueries({ queryKey: ["deployments", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not save the stack"))),
  });
  return (
    <Card
      title={t("Compose source")}
      description={t("Where the compose file comes from. Deploy again after changing it.")}
    >
      <form
        className="space-y-4"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate(false);
        }}
      >
        <div className="grid gap-4 md:grid-cols-2">
          <PickField
            label={t("Source")}
            options={STACK_SOURCES}
            value={source}
            onPick={setSource}
          />
          <Field
            label={t("Main service (default domain target)")}
            presets={
              <QuickPick
                label={t("Main service presets")}
                options={MAIN_SERVICES}
                value={mainService}
                onPick={setMainService}
              />
            }
          >
            <TextInput
              value={mainService}
              placeholder="web"
              onChange={(event) => setMainService(event.target.value)}
            />
          </Field>
        </div>
        {source === "inline" ? (
          <Field label="docker-compose.yml" presets={<YamlStarters yaml={yaml} onPick={setYaml} />}>
            <TextArea
              className="min-h-80"
              spellCheck={false}
              value={yaml}
              placeholder={COMPOSE_EXAMPLE}
              onChange={(event) => setYaml(event.target.value)}
            />
          </Field>
        ) : (
          <GitFields git={git} providers={providers} onChange={setGit} />
        )}
        <fieldset className="grid gap-2.5 rounded-alert border border-line p-3.5">
          <legend className="px-1.5 text-xs font-medium text-muted">{t("When deploying")}</legend>
          <label className="flex items-start gap-2.5 text-sm">
            <input
              type="checkbox"
              className="mt-0.5"
              checked={buildOnDeploy}
              onChange={(event) => setBuildOnDeploy(event.target.checked)}
            />
            <span>
              <span className="font-medium text-ink">{t("Build images on every deploy")}</span>
              <span className="block text-muted">
                {hasBuilds
                  ? t(
                      "Services with a build section are rebuilt each time, so new code is picked up. Slower than reusing the images from the last build.",
                    )
                  : t(
                      "Only matters for compose files with a build section. Without it, a deploy reuses images that were built earlier and never picks up new code.",
                    )}
              </span>
            </span>
          </label>
          {source === "git" ? (
            <label className="flex items-start gap-2.5 text-sm">
              <input
                type="checkbox"
                className="mt-0.5"
                checked={deployOnPush}
                onChange={(event) => setDeployOnPush(event.target.checked)}
              />
              <span>
                <span className="font-medium text-ink">{t("Deploy when I push to the branch")}</span>
                <span className="block text-muted">
                  {t(
                    "Needs a Git provider connected with the GitHub button; pushes reach the panel through its webhook.",
                  )}
                </span>
              </span>
            </label>
          ) : null}
        </fieldset>
        <p className="text-sm text-muted">
          {t(
            "Use {variable} for values from the Environment tab; Rudder writes them to a mode 0600 {file} next to the generated file.",
            {
              variable: <span className="font-mono">{"${NAME}"}</span>,
              file: <span className="font-mono">.env</span>,
            },
          )}
        </p>
        <div className="flex gap-2">
          <Button type="submit" disabled={save.isPending}>
            {t("Save")}
          </Button>
          <Button
            variant="secondary"
            icon="rocket"
            type="button"
            disabled={save.isPending}
            onClick={() => save.mutate(true)}
          >
            {t("Save and deploy")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

function GitFields({
  git,
  providers,
  onChange,
}: {
  git: StackGit;
  providers: GitProvider[];
  onChange: (git: StackGit) => void;
}) {
  const t = useT();
  return (
    <div className="grid gap-4 md:grid-cols-2">
      <Field label={t("Provider")}>
        <Select
          required
          value={git.provider_id}
          onChange={(event) => onChange({ ...git, provider_id: event.target.value })}
        >
          <option value="">{t("Choose")}</option>
          {providers.map((provider) => (
            <option key={provider.id} value={provider.id} disabled={!providerReady(provider)}>
              {providerLabel(provider, t)}
            </option>
          ))}
        </Select>
      </Field>
      <Field label={t("Repository")}>
        <TextInput
          required
          value={git.repo}
          placeholder="https://github.com/org/stack.git"
          onChange={(event) => onChange({ ...git, repo: event.target.value })}
        />
      </Field>
      <Field
        label={t("Branch")}
        presets={
          <QuickPick
            label={t("Branch presets")}
            options={BRANCHES}
            value={git.branch}
            onPick={(branch) => onChange({ ...git, branch })}
          />
        }
      >
        <TextInput
          required
          value={git.branch}
          onChange={(event) => onChange({ ...git, branch: event.target.value })}
        />
      </Field>
      <Field
        label={t("Compose file path")}
        presets={
          <QuickPick
            label={t("Compose file presets")}
            options={COMPOSE_FILES}
            value={git.compose_file}
            onPick={(compose_file) => onChange({ ...git, compose_file })}
          />
        }
      >
        <TextInput
          required
          value={git.compose_file}
          onChange={(event) => onChange({ ...git, compose_file: event.target.value })}
        />
      </Field>
    </div>
  );
}

export function StackLogs({ serviceId }: { serviceId: string }) {
  const t = useT();
  const stack = useStack(serviceId);
  const names = stack.data?.services.map((service) => service.name) ?? [];
  const [selected, setSelected] = useState("");
  const [tail, setTail] = useState("200");
  const service = selected || names[0] || "";
  const logs = useQuery({
    queryKey: ["stack-logs", serviceId, service, tail],
    queryFn: () =>
      api<Schemas["StackLogsView"]>(
        `/api/v1/services/${serviceId}/stack/logs?service=${encodeURIComponent(service)}&tail=${tail}`,
      ),
    enabled: Boolean(service),
    refetchInterval: 5000,
    retry: false,
  });
  return (
    <Card
      title={t("Service logs")}
      description={t("Live output of one container in the stack. Refreshes every 5 seconds.")}
      actions={
        <>
          <div className="w-44">
            <Select
              aria-label={t("Compose service")}
              value={service}
              onChange={(event) => setSelected(event.target.value)}
            >
              {names.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </Select>
          </div>
          <Segmented
            label={t("Lines")}
            value={tail}
            onChange={setTail}
            options={[
              ["100", "100"],
              ["200", "200"],
              ["1000", "1000"],
            ]}
          />
        </>
      }
    >
      {!service ? (
        <Empty>{t("Deploy the stack to see logs.")}</Empty>
      ) : logs.error ? (
        <Empty>{errorMessage(logs.error, t("No logs available"))}</Empty>
      ) : (
        <LogPre empty={t("No output yet.")}>{logs.data?.logs}</LogPre>
      )}
    </Card>
  );
}

export function CreateStackFields({
  source,
  yaml,
  git,
  providers,
  onSource,
  onYaml,
  onGit,
}: {
  source: string;
  yaml: string;
  git: StackGit;
  providers: GitProvider[];
  onSource: (source: string) => void;
  onYaml: (yaml: string) => void;
  onGit: (git: StackGit) => void;
}) {
  const t = useT();
  return (
    <div className="space-y-3 md:col-span-2">
      <PickField
        label={t("Compose source")}
        options={STACK_SOURCES}
        value={source}
        onPick={onSource}
      />
      {source === "inline" ? (
        <Field label="docker-compose.yml" presets={<YamlStarters yaml={yaml} onPick={onYaml} />}>
          <TextArea
            className="min-h-60"
            spellCheck={false}
            required
            value={yaml}
            placeholder={COMPOSE_EXAMPLE}
            onChange={(event) => onYaml(event.target.value)}
          />
        </Field>
      ) : (
        <GitFields git={git} providers={providers} onChange={onGit} />
      )}
    </div>
  );
}
