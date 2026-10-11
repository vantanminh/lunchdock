import { useMutation, useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { api } from "../api";
import { Badge, Button, Card, Empty, Field, Select, TextInput } from "../components/ui";
import { errorMessage } from "../format";
import type { Schemas } from "../types";

type Template = Schemas["TemplateView"];

interface Named {
  id: string;
  name: string;
}

export function TemplatesPage() {
  const templates = useQuery({
    queryKey: ["templates"],
    queryFn: () => api<Template[]>("/api/v1/templates"),
  });
  const [search, setSearch] = useState("");
  const [category, setCategory] = useState("");
  const [chosen, setChosen] = useState<Template | null>(null);
  const categories = useMemo(
    () => [...new Set((templates.data ?? []).map((t) => t.category))].sort(),
    [templates.data],
  );
  const needle = search.trim().toLowerCase();
  const shown = (templates.data ?? []).filter(
    (template) =>
      (!category || template.category === category) &&
      (!needle ||
        [template.name, template.description, template.slug, ...template.tags]
          .join(" ")
          .toLowerCase()
          .includes(needle)),
  );

  return (
    <>
      <h1 className="text-2xl font-semibold">Templates</h1>
      <p className="text-sm text-muted">
        One-click apps that run as compose stacks. Secrets left empty are generated and stored
        encrypted.
      </p>
      {chosen ? (
        <DeployForm key={chosen.slug} template={chosen} onClose={() => setChosen(null)} />
      ) : null}
      <div className="flex flex-wrap gap-3">
        <div className="w-full max-w-sm">
          <TextInput
            type="search"
            aria-label="Search templates"
            placeholder="Search by name, tag, or description"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
          />
        </div>
        <div className="w-48">
          <Select
            aria-label="Category"
            value={category}
            onChange={(event) => setCategory(event.target.value)}
          >
            <option value="">All categories</option>
            {categories.map((item) => (
              <option key={item} value={item}>
                {item}
              </option>
            ))}
          </Select>
        </div>
      </div>
      {templates.error ? <Empty>{errorMessage(templates.error, "Could not load")}</Empty> : null}
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
        {shown.map((template) => (
          <section
            key={template.slug}
            className="flex flex-col rounded-xl border border-line bg-panel p-5 shadow-sm"
          >
            <div className="flex items-start justify-between gap-2">
              <h2 className="text-lg font-semibold">{template.name}</h2>
              <Badge value={template.category} />
            </div>
            <p className="mt-2 flex-1 text-sm text-muted">{template.description}</p>
            <p className="mt-3 font-mono text-xs text-muted">{template.images.join(" · ")}</p>
            <div className="mt-2 flex flex-wrap gap-1">
              {template.tags.map((tag) => (
                <span key={tag} className="rounded bg-paper px-1.5 py-0.5 text-xs text-muted">
                  {tag}
                </span>
              ))}
            </div>
            <div className="mt-4 flex items-center gap-3">
              <Button onClick={() => setChosen(template)}>Deploy</Button>
              {template.website ? (
                <a
                  className="text-sm text-accent"
                  href={template.website}
                  target="_blank"
                  rel="noreferrer"
                >
                  Website
                </a>
              ) : null}
            </div>
          </section>
        ))}
      </div>
      {templates.data && shown.length === 0 ? <Empty>No templates match.</Empty> : null}
    </>
  );
}

function DeployForm({ template, onClose }: { template: Template; onClose: () => void }) {
  const projects = useQuery({
    queryKey: ["projects"],
    queryFn: () => api<Named[]>("/api/v1/projects"),
  });
  const servers = useQuery({
    queryKey: ["servers"],
    queryFn: () => api<Named[]>("/api/v1/servers"),
  });
  const [projectId, setProjectId] = useState("");
  const project = projectId || projects.data?.[0]?.id || "";
  const environments = useQuery({
    queryKey: ["environments", project],
    queryFn: () => api<Named[]>(`/api/v1/projects/${project}/environments`),
    enabled: Boolean(project),
  });
  const [environmentId, setEnvironmentId] = useState("");
  const environment = environmentId || environments.data?.[0]?.id || "";
  const [serverId, setServerId] = useState("");
  const server = serverId || servers.data?.[0]?.id || "";
  const [name, setName] = useState(template.slug);
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(template.variables.map((v) => [v.name, v.default ?? ""])),
  );
  const [result, setResult] = useState<Schemas["TemplateDeployed"] | null>(null);
  const deploy = useMutation({
    mutationFn: () =>
      api<Schemas["TemplateDeployed"]>(
        `/api/v1/environments/${environment}/templates/${template.slug}/deploy`,
        {
          method: "POST",
          body: JSON.stringify({
            name,
            server_id: server,
            values: Object.fromEntries(Object.entries(values).filter(([, value]) => value !== "")),
          }),
        },
      ),
    onSuccess: (deployed) => {
      setResult(deployed);
      toast.success(`${template.name} is deploying`);
    },
    onError: (error) => toast.error(errorMessage(error, "Deploy failed")),
  });

  if (result) {
    return (
      <Card title={`${template.name} deployed`}>
        {result.domain ? (
          <p className="text-sm">
            Routed at <span className="font-mono">https://{result.domain}</span> once DNS points at
            this server.
          </p>
        ) : null}
        {result.generated.length > 0 ? (
          <div className="mt-3">
            <p className="text-sm font-medium">
              Generated secrets — copy them now, they are not shown again:
            </p>
            <ul className="mt-2 space-y-1 rounded-md bg-paper p-3 font-mono text-xs break-all">
              {result.generated.map((item) => (
                <li key={item.key}>
                  {item.key}={item.value}
                </li>
              ))}
            </ul>
          </div>
        ) : null}
        {template.notes ? <p className="mt-3 text-sm text-muted">{template.notes}</p> : null}
        <div className="mt-4 flex gap-2">
          <Link
            className="inline-flex h-10 items-center rounded-md bg-accent px-3 text-sm font-medium text-accent-ink"
            to={`/services/${result.service.id}`}
          >
            Open service
          </Link>
          <Button quiet onClick={onClose}>
            Close
          </Button>
        </div>
      </Card>
    );
  }

  return (
    <Card title={`Deploy ${template.name}`}>
      {template.notes ? <p className="mb-3 text-sm text-muted">{template.notes}</p> : null}
      <form
        className="grid gap-3 md:grid-cols-2"
        onSubmit={(event) => {
          event.preventDefault();
          deploy.mutate();
        }}
      >
        <Field label="Project">
          <Select
            value={project}
            onChange={(event) => {
              setProjectId(event.target.value);
              setEnvironmentId("");
            }}
          >
            {(projects.data ?? []).map((item) => (
              <option key={item.id} value={item.id}>
                {item.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Environment">
          <Select value={environment} onChange={(event) => setEnvironmentId(event.target.value)}>
            {(environments.data ?? []).map((item) => (
              <option key={item.id} value={item.id}>
                {item.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Server">
          <Select value={server} onChange={(event) => setServerId(event.target.value)}>
            {(servers.data ?? []).map((item) => (
              <option key={item.id} value={item.id}>
                {item.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Service name">
          <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        {template.variables.map((variable) => (
          <Field
            key={variable.name}
            label={`${variable.label}${variable.required ? " *" : ""}${variable.kind === "secret" ? " (empty = generate)" : ""}`}
          >
            {variable.kind === "boolean" ? (
              <Select
                value={values[variable.name] ?? ""}
                onChange={(event) => setValues({ ...values, [variable.name]: event.target.value })}
              >
                <option value="true">true</option>
                <option value="false">false</option>
              </Select>
            ) : (
              <TextInput
                required={variable.required}
                type={
                  variable.kind === "secret"
                    ? "password"
                    : variable.kind === "email"
                      ? "email"
                      : variable.kind === "number"
                        ? "number"
                        : "text"
                }
                autoComplete="off"
                placeholder={variable.kind === "domain" ? "app.example.com" : variable.description}
                value={values[variable.name] ?? ""}
                onChange={(event) => setValues({ ...values, [variable.name]: event.target.value })}
              />
            )}
          </Field>
        ))}
        <div className="flex items-end gap-2 md:col-span-2">
          <Button type="submit" disabled={!environment || !server || deploy.isPending}>
            Deploy {template.name}
          </Button>
          <Button quiet type="button" onClick={onClose}>
            Cancel
          </Button>
        </div>
        {projects.data?.length === 0 ? (
          <p className="text-sm text-muted md:col-span-2">
            Create a project and environment first under Projects.
          </p>
        ) : null}
      </form>
    </Card>
  );
}
