import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { CreateStackFields } from "../components/stacks";
import { Button, Card, Field, TextInput } from "../components/ui";
import type { Schemas } from "../types";

type Project = Schemas["ProjectView"];

type Environment = Schemas["EnvironmentView"];

type Service = Schemas["ServiceView"];

type Server = Schemas["ServerView"];

export function ProjectsPage() {
  const queryClient = useQueryClient();
  const projects = useQuery({
    queryKey: ["projects"],
    queryFn: () => api<Project[]>("/api/v1/projects"),
  });
  const servers = useQuery({
    queryKey: ["servers"],
    queryFn: () => api<Server[]>("/api/v1/servers"),
  });
  const [name, setName] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [environmentName, setEnvironmentName] = useState("prod");
  const [serviceName, setServiceName] = useState("");
  const [kind, setKind] = useState("application");
  const [serverId, setServerId] = useState("");
  const [image, setImage] = useState("");
  const [engine, setEngine] = useState("postgres");
  const [version, setVersion] = useState("");
  const [stackSource, setStackSource] = useState("inline");
  const [stackYaml, setStackYaml] = useState("");
  const [stackGit, setStackGit] = useState<Schemas["StackGit"]>({
    provider_id: "",
    repo: "",
    branch: "main",
    compose_file: "docker-compose.yml",
  });
  const [mainService, setMainService] = useState("");
  const providers = useQuery({
    queryKey: ["git-providers"],
    queryFn: () => api<Schemas["GitProviderView"][]>("/api/v1/git-providers"),
    enabled: kind === "compose",
  });
  const engines = useQuery({
    queryKey: ["database-engines"],
    queryFn: () => api<Schemas["DatabaseEngineView"][]>("/api/v1/database-engines"),
    enabled: kind === "database",
  });
  const engineVersions = engines.data?.find((item) => item.engine === engine)?.versions ?? [];

  const environments = useQuery({
    queryKey: ["environments", selected],
    queryFn: () => api<Environment[]>(`/api/v1/projects/${selected}/environments`),
    enabled: Boolean(selected),
  });
  const environmentId = environments.data?.[0]?.id;
  const services = useQuery({
    queryKey: ["services", environmentId],
    queryFn: () => api<Service[]>(`/api/v1/environments/${environmentId}/services`),
    enabled: Boolean(environmentId),
  });

  const createProject = useMutation({
    mutationFn: () =>
      api<Project>("/api/v1/projects", {
        method: "POST",
        body: JSON.stringify({ name }),
      }),
    onSuccess: async (project) => {
      setName("");
      setSelected(project.id);
      await queryClient.invalidateQueries({ queryKey: ["projects"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not create project"),
  });

  const createEnvironment = useMutation({
    mutationFn: () =>
      api(`/api/v1/projects/${selected}/environments`, {
        method: "POST",
        body: JSON.stringify({ name: environmentName }),
      }),
    onSuccess: async () => {
      toast.success("Environment created");
      await queryClient.invalidateQueries({ queryKey: ["environments", selected] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not create environment"),
  });

  const createService = useMutation({
    mutationFn: () =>
      kind === "database"
        ? api(`/api/v1/environments/${environmentId}/databases`, {
            method: "POST",
            body: JSON.stringify({
              name: serviceName,
              engine,
              version: version || engineVersions[0],
              server_id: serverId || servers.data?.[0]?.id,
            }),
          })
        : kind === "compose"
          ? api(`/api/v1/environments/${environmentId}/stacks`, {
              method: "POST",
              body: JSON.stringify({
                name: serviceName,
                server_id: serverId || servers.data?.[0]?.id,
                source: stackSource,
                compose_yaml: stackSource === "inline" ? stackYaml : undefined,
                git: stackSource === "git" ? stackGit : undefined,
                main_service: mainService || undefined,
              }),
            })
          : api(`/api/v1/environments/${environmentId}/services`, {
              method: "POST",
              body: JSON.stringify({
                name: serviceName,
                kind,
                server_id: serverId || servers.data?.[0]?.id,
                spec: image ? { image } : {},
              }),
            }),
    onSuccess: async () => {
      setServiceName("");
      toast.success(
        kind === "database"
          ? "Database created with a generated password"
          : kind === "compose"
            ? "Stack saved; deploy it from its page"
            : "Service created",
      );
      await queryClient.invalidateQueries({ queryKey: ["services", environmentId] });
    },
    onError: async (error) => {
      toast.error(error instanceof ApiError ? error.message : "Could not create service");
      await queryClient.invalidateQueries({ queryKey: ["services", environmentId] });
    },
  });

  function onProject(event: FormEvent) {
    event.preventDefault();
    createProject.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">Projects</h1>
      <Card title="New project">
        <form className="flex flex-wrap items-end gap-3" onSubmit={onProject}>
          <Field label="Name">
            <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
          </Field>
          <Button type="submit">Create</Button>
        </form>
      </Card>
      <div className="grid gap-4 md:grid-cols-[220px_1fr]">
        <Card title="All">
          <ul className="space-y-1">
            {(projects.data ?? []).map((project) => (
              <li key={project.id}>
                <button
                  type="button"
                  className={`w-full rounded-md px-2 py-1 text-left text-sm ${selected === project.id ? "bg-paper font-medium" : ""}`}
                  onClick={() => setSelected(project.id)}
                >
                  {project.name}
                </button>
              </li>
            ))}
          </ul>
        </Card>
        {selected ? (
          <div className="space-y-4">
            <Card title="Environment">
              <form
                className="flex flex-wrap items-end gap-3"
                onSubmit={(event) => {
                  event.preventDefault();
                  createEnvironment.mutate();
                }}
              >
                <Field label="Name">
                  <TextInput
                    required
                    value={environmentName}
                    onChange={(event) => setEnvironmentName(event.target.value)}
                  />
                </Field>
                <Button type="submit">Add environment</Button>
              </form>
              <p className="mt-3 text-sm text-muted">
                {(environments.data ?? []).map((environment) => environment.name).join(", ") ||
                  "None yet"}
              </p>
            </Card>
            <Card title="Service">
              <form
                className="grid gap-3 md:grid-cols-2"
                onSubmit={(event) => {
                  event.preventDefault();
                  createService.mutate();
                }}
              >
                <Field label="Name">
                  <TextInput
                    required
                    value={serviceName}
                    onChange={(event) => setServiceName(event.target.value)}
                  />
                </Field>
                <Field label="Kind">
                  <select
                    className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                    value={kind}
                    onChange={(event) => setKind(event.target.value)}
                  >
                    <option value="application">application</option>
                    <option value="database">database</option>
                    <option value="compose">compose stack</option>
                  </select>
                </Field>
                <Field label="Server">
                  <select
                    className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                    value={serverId}
                    onChange={(event) => setServerId(event.target.value)}
                  >
                    <option value="">Select</option>
                    {(servers.data ?? []).map((server) => (
                      <option key={server.id} value={server.id}>
                        {server.name}
                      </option>
                    ))}
                  </select>
                </Field>
                {kind === "database" ? (
                  <>
                    <Field label="Engine">
                      <select
                        className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                        value={engine}
                        onChange={(event) => {
                          setEngine(event.target.value);
                          setVersion("");
                        }}
                      >
                        {(engines.data ?? []).map((item) => (
                          <option key={item.engine} value={item.engine}>
                            {item.engine}
                          </option>
                        ))}
                      </select>
                    </Field>
                    <Field label="Version">
                      <select
                        className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
                        value={version || engineVersions[0] || ""}
                        onChange={(event) => setVersion(event.target.value)}
                      >
                        {engineVersions.map((item) => (
                          <option key={item} value={item}>
                            {item}
                          </option>
                        ))}
                      </select>
                    </Field>
                  </>
                ) : kind === "compose" ? (
                  <>
                    <Field label="Main service (receives domains)">
                      <TextInput
                        value={mainService}
                        placeholder="web"
                        onChange={(event) => setMainService(event.target.value)}
                      />
                    </Field>
                    <CreateStackFields
                      source={stackSource}
                      yaml={stackYaml}
                      git={stackGit}
                      providers={providers.data ?? []}
                      onSource={setStackSource}
                      onYaml={setStackYaml}
                      onGit={setStackGit}
                    />
                  </>
                ) : (
                  <Field label="Image">
                    <TextInput value={image} onChange={(event) => setImage(event.target.value)} />
                  </Field>
                )}
                <Button type="submit" disabled={!environmentId}>
                  Add service
                </Button>
              </form>
              <p className="mt-3 text-sm text-muted">
                Or start from a ready-made app in{" "}
                <Link className="text-accent" to="/templates">
                  Templates
                </Link>
                .
              </p>
              <ul className="mt-4 divide-y divide-line text-sm">
                {(services.data ?? []).map((service) => (
                  <li key={service.id} className="py-2">
                    <Link className="font-medium text-accent" to={`/services/${service.id}`}>
                      {service.name}
                    </Link>{" "}
                    · {service.kind} · {service.status}
                  </li>
                ))}
              </ul>
            </Card>
          </div>
        ) : (
          <p className="text-sm text-muted">Select a project.</p>
        )}
      </div>
    </>
  );
}
