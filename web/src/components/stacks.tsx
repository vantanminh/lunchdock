import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { toast } from "sonner";
import { api } from "../api";
import { errorMessage } from "../format";
import type { Schemas } from "../types";
import { Badge, Button, Card, Empty, Field, Select, TextArea, TextInput } from "./ui";

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
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const stack = useStack(serviceId);
  const action = useMutation({
    mutationFn: (name: "up" | "down" | "restart" | "pull") =>
      api<Schemas["DeploymentView"]>(`/api/v1/services/${serviceId}/stack/${name}`, {
        method: "POST",
      }),
    onSuccess: async (_deployment, name) => {
      toast.success(`docker compose ${name} started; follow it under Deployments`);
      await queryClient.invalidateQueries({ queryKey: ["stack", serviceId] });
      await queryClient.invalidateQueries({ queryKey: ["deployments", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, "Stack action failed")),
  });
  const remove = useMutation({
    mutationFn: (volumes: boolean) =>
      api(`/api/v1/services/${serviceId}/stack?volumes=${volumes}`, { method: "DELETE" }),
    onSuccess: () => {
      toast.success("Stack deleted");
      navigate("/projects");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not delete the stack")),
  });
  const view = stack.data;
  if (stack.error) {
    return <Empty>{errorMessage(stack.error, "Could not load the stack")}</Empty>;
  }
  if (!view) {
    return <Empty>Loading stack…</Empty>;
  }
  const busy = view.service.status === "deploying" || action.isPending;
  return (
    <>
      <Card title="Stack">
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span className="text-muted">Project</span>
          <span className="font-mono">{view.project}</span>
          {view.template ? <Badge value={`template: ${view.template}`} /> : null}
          <span className="text-muted">· {view.source === "git" ? "Git" : "pasted YAML"}</span>
        </div>
        <div className="mt-4 flex flex-wrap gap-2">
          <Button disabled={busy} onClick={() => action.mutate("up")}>
            Deploy (up)
          </Button>
          <Button quiet disabled={busy} onClick={() => action.mutate("restart")}>
            Restart
          </Button>
          <Button quiet disabled={busy} onClick={() => action.mutate("pull")}>
            Pull images
          </Button>
          <Button quiet disabled={busy} onClick={() => action.mutate("down")}>
            Stop (down)
          </Button>
          <Button
            quiet
            className="ml-auto"
            disabled={busy}
            onClick={() => {
              if (!window.confirm(`Delete ${view.service.name}? Containers are removed.`)) {
                return;
              }
              const volumes = window.confirm(
                "Also delete the stack's named volumes? This erases its data. Cancel keeps them.",
              );
              remove.mutate(volumes);
            }}
          >
            Delete stack
          </Button>
        </div>
        {view.missing_variables.length > 0 ? (
          <p className="mt-4 text-sm text-amber-700 dark:text-amber-300">
            Not set under Environment: {view.missing_variables.join(", ")}
          </p>
        ) : null}
        {view.warnings.length > 0 ? (
          <ul className="mt-3 list-disc space-y-1 pl-5 text-sm text-muted">
            {view.warnings.map((warning) => (
              <li key={warning}>{warning}</li>
            ))}
          </ul>
        ) : null}
      </Card>
      <Card title="Services">
        {view.services.length === 0 ? (
          <Empty>The compose file is read from Git on the first deploy.</Empty>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-left text-sm">
              <thead className="text-muted">
                <tr>
                  <th className="py-2 pr-4 font-medium">Service</th>
                  <th className="py-2 pr-4 font-medium">Image</th>
                  <th className="py-2 pr-4 font-medium">Container</th>
                  <th className="py-2 pr-4 font-medium">State</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-line">
                {view.services.map((service) => (
                  <tr key={service.name}>
                    <td className="py-2 pr-4 font-medium">
                      {service.name}
                      {service.routed ? (
                        <span className="ml-2 text-xs text-muted">routed</span>
                      ) : null}
                    </td>
                    <td className="py-2 pr-4 font-mono text-xs">{service.image ?? "build"}</td>
                    <td className="py-2 pr-4 font-mono text-xs">{service.container}</td>
                    <td className="py-2 pr-4">
                      <Badge
                        value={
                          service.running == null
                            ? "unreachable"
                            : service.running
                              ? "running"
                              : "stopped"
                        }
                      />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
      <Card title="Generated compose file">
        <p className="mb-3 text-sm text-muted">
          What Rudder runs{view.compose_path ? ` from ${view.compose_path}` : ""}: your file with
          the project name, rudder labels, and the rudder-proxy network for routed services.
        </p>
        <pre className="max-h-[28rem] overflow-auto rounded-md bg-paper p-3 text-xs">
          {view.generated_yaml || "Not generated yet."}
        </pre>
      </Card>
    </>
  );
}

export function StackSourceEditor({ serviceId }: { serviceId: string }) {
  const stack = useStack(serviceId);
  const providers = useQuery({
    queryKey: ["git-providers"],
    queryFn: () => api<GitProvider[]>("/api/v1/git-providers"),
  });
  if (!stack.data) {
    return <Empty>{stack.error ? errorMessage(stack.error, "Could not load") : "Loading…"}</Empty>;
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

function SourceForm({
  serviceId,
  initial,
  providers,
}: {
  serviceId: string;
  initial: StackView;
  providers: GitProvider[];
}) {
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
  const save = useMutation({
    mutationFn: (deploy: boolean) =>
      api<StackView>(`/api/v1/services/${serviceId}/stack`, {
        method: "PUT",
        body: JSON.stringify({
          source,
          compose_yaml: source === "inline" ? yaml : undefined,
          git: source === "git" ? git : undefined,
          main_service: mainService || undefined,
        }),
      }).then(async (view) => {
        if (deploy) {
          await api(`/api/v1/services/${serviceId}/stack/up`, { method: "POST" });
        }
        return view;
      }),
    onSuccess: async (_view, deploy) => {
      toast.success(deploy ? "Saved; docker compose up started" : "Compose source saved");
      await queryClient.invalidateQueries({ queryKey: ["stack", serviceId] });
      await queryClient.invalidateQueries({ queryKey: ["deployments", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save the stack")),
  });
  return (
    <Card title="Compose source">
      <form
        className="space-y-3"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate(false);
        }}
      >
        <div className="grid gap-3 md:grid-cols-2">
          <Field label="Source">
            <Select value={source} onChange={(event) => setSource(event.target.value)}>
              <option value="inline">Paste YAML</option>
              <option value="git">Git repository (local server)</option>
            </Select>
          </Field>
          <Field label="Main service (default domain target)">
            <TextInput
              value={mainService}
              placeholder="web"
              onChange={(event) => setMainService(event.target.value)}
            />
          </Field>
        </div>
        {source === "inline" ? (
          <Field label="docker-compose.yml">
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
        <p className="text-sm text-muted">
          Use <span className="font-mono">{"${NAME}"}</span> for values from the Environment tab;
          Rudder writes them to a mode 0600 <span className="font-mono">.env</span> next to the
          generated file.
        </p>
        <div className="flex gap-2">
          <Button type="submit" disabled={save.isPending}>
            Save
          </Button>
          <Button quiet type="button" disabled={save.isPending} onClick={() => save.mutate(true)}>
            Save and deploy
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
  return (
    <div className="grid gap-3 md:grid-cols-2">
      <Field label="Provider">
        <Select
          required
          value={git.provider_id}
          onChange={(event) => onChange({ ...git, provider_id: event.target.value })}
        >
          <option value="">Choose</option>
          {providers.map((provider) => (
            <option key={provider.id} value={provider.id}>
              {provider.name} ({provider.kind})
            </option>
          ))}
        </Select>
      </Field>
      <Field label="Repository">
        <TextInput
          required
          value={git.repo}
          placeholder="https://github.com/org/stack.git"
          onChange={(event) => onChange({ ...git, repo: event.target.value })}
        />
      </Field>
      <Field label="Branch">
        <TextInput
          required
          value={git.branch}
          onChange={(event) => onChange({ ...git, branch: event.target.value })}
        />
      </Field>
      <Field label="Compose file path">
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
  const stack = useStack(serviceId);
  const names = stack.data?.services.map((service) => service.name) ?? [];
  const [selected, setSelected] = useState("");
  const [tail, setTail] = useState(200);
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
    <Card title="Service logs">
      <div className="mb-3 flex flex-wrap items-center gap-2 text-sm">
        <div className="w-48">
          <Select value={service} onChange={(event) => setSelected(event.target.value)}>
            {names.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </Select>
        </div>
        <span className="text-muted">Lines</span>
        {[100, 200, 1000].map((count) => (
          <Button key={count} quiet={tail !== count} onClick={() => setTail(count)}>
            {count}
          </Button>
        ))}
      </div>
      {!service ? (
        <Empty>Deploy the stack to see logs.</Empty>
      ) : logs.error ? (
        <Empty>{errorMessage(logs.error, "No logs available")}</Empty>
      ) : (
        <pre className="max-h-[32rem] overflow-auto rounded-md bg-paper p-3 text-xs whitespace-pre-wrap">
          {logs.data?.logs || "No output yet."}
        </pre>
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
  return (
    <div className="space-y-3 md:col-span-2">
      <Field label="Compose source">
        <Select value={source} onChange={(event) => onSource(event.target.value)}>
          <option value="inline">Paste YAML</option>
          <option value="git">Git repository (local server)</option>
        </Select>
      </Field>
      {source === "inline" ? (
        <Field label="docker-compose.yml">
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
