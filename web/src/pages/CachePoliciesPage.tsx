import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { hitRatio, ruleSummary, usePolicies } from "../components/cache";
import {
  Button,
  Card,
  Empty,
  Field,
  PickField,
  QuickPick,
  TextArea,
  TextInput,
} from "../components/ui";
import { errorMessage, formatBytes } from "../format";
import { useT } from "../i18n";
import { CACHE_MEMORY_MIB, CACHE_OBJECT_MIB } from "../presets";
import type { Schemas } from "../types";

type Policy = Schemas["CachePolicyView"];
type Preset = Schemas["CachePresetView"];
type Rule = Schemas["CacheRule"];

const MIB = 1024 * 1024;

function toMiB(bytes: number | null | undefined): string {
  return bytes == null ? "" : String(Math.round(bytes / MIB));
}

function fromMiB(value: string): number | null {
  const trimmed = value.trim();
  return trimmed ? Math.round(Number(trimmed) * MIB) : null;
}

export function CachePoliciesPage() {
  const policies = usePolicies();
  const presets = useQuery({
    queryKey: ["cache-presets"],
    queryFn: () => api<Preset[]>("/api/v1/cache-presets"),
  });
  const [editing, setEditing] = useState<Policy | "new" | null>(null);

  return (
    <>
      <div>
        <h1 className="text-2xl font-semibold">Cache</h1>
        <p className="text-sm text-muted">
          Policies are ordered rules: the first rule that matches a request decides whether it is
          cached and for how long. Turn caching on per domain in a service&apos;s Cache tab.
        </p>
      </div>
      <CacheServers />
      <Card title="Policies">
        <div className="mb-4">
          <Button onClick={() => setEditing("new")}>New policy</Button>
        </div>
        {policies.data?.length === 0 ? <Empty>No policies yet. Start from a preset.</Empty> : null}
        <ul className="space-y-3">
          {(policies.data ?? []).map((policy) => (
            <li key={policy.id} className="rounded-md border border-line p-3 text-sm">
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium">{policy.name}</span>
                <span className="text-muted">
                  {policy.domains} domain{policy.domains === 1 ? "" : "s"} · memory{" "}
                  {policy.memory_max_bytes ? formatBytes(policy.memory_max_bytes) : "default"} · max
                  object {policy.max_object_bytes ? formatBytes(policy.max_object_bytes) : "16 MiB"}
                </span>
                <Button quiet className="ml-auto" onClick={() => setEditing(policy)}>
                  Edit
                </Button>
              </div>
              {policy.description ? <p className="mt-1 text-muted">{policy.description}</p> : null}
              <ol className="mt-2 list-decimal space-y-0.5 pl-5 text-muted">
                {policy.rules.map((rule, index) => (
                  <li key={index}>
                    <span className="text-ink">{rule.name || `Rule ${index + 1}`}</span>:{" "}
                    {ruleSummary(rule)}
                  </li>
                ))}
              </ol>
            </li>
          ))}
        </ul>
      </Card>
      {editing ? (
        <PolicyEditor
          key={editing === "new" ? "new" : editing.id}
          policy={editing === "new" ? null : editing}
          presets={presets.data ?? []}
          onDone={() => setEditing(null)}
        />
      ) : null}
    </>
  );
}

function PolicyEditor({
  policy,
  presets,
  onDone,
}: {
  policy: Policy | null;
  presets: Preset[];
  onDone: () => void;
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const [name, setName] = useState(policy?.name ?? "");
  const [description, setDescription] = useState(policy?.description ?? "");
  const [memory, setMemory] = useState(toMiB(policy?.memory_max_bytes));
  const [maxObject, setMaxObject] = useState(toMiB(policy?.max_object_bytes));
  const [preset, setPreset] = useState(policy ? "" : (presets[0]?.slug ?? "static-assets"));
  const [rulesText, setRulesText] = useState(
    JSON.stringify(policy?.rules ?? presets[0]?.rules ?? [], null, 2),
  );

  function choosePreset(slug: string) {
    setPreset(slug);
    const found = presets.find((item) => item.slug === slug);
    if (found) {
      setRulesText(JSON.stringify(found.rules, null, 2));
      if (!name) setName(found.name);
      if (!description) setDescription(found.description);
    }
  }

  const save = useMutation({
    mutationFn: () => {
      let rules: Rule[];
      try {
        rules = JSON.parse(rulesText) as Rule[];
      } catch {
        throw new Error("Rules must be valid JSON");
      }
      const body = JSON.stringify({
        name,
        description,
        memory_max_bytes: fromMiB(memory),
        max_object_bytes: fromMiB(maxObject),
        rules,
      });
      return policy
        ? api(`/api/v1/cache-policies/${policy.id}`, { method: "PUT", body })
        : api("/api/v1/cache-policies", { method: "POST", body });
    },
    onSuccess: async () => {
      toast.success(policy ? "Policy saved and published" : "Policy created");
      await queryClient.invalidateQueries({ queryKey: ["cache-policies"] });
      onDone();
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save the policy")),
  });

  const remove = useMutation({
    mutationFn: () => api(`/api/v1/cache-policies/${policy?.id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success("Policy deleted");
      await queryClient.invalidateQueries({ queryKey: ["cache-policies"] });
      onDone();
    },
    onError: (error) => toast.error(errorMessage(error, "Could not delete the policy")),
  });

  return (
    <Card title={policy ? `Edit ${policy.name}` : "New policy"}>
      <form
        className="grid gap-3 md:grid-cols-2"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        {!policy ? (
          <div className="md:col-span-2">
            <PickField
              label="Start from preset"
              hint={presets.find((item) => item.slug === preset)?.description}
              options={presets.map((item) => ({ label: item.name, value: item.slug }))}
              value={preset}
              onPick={choosePreset}
            />
          </div>
        ) : null}
        <Field label="Name">
          <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <Field label="Description">
          <TextInput value={description} onChange={(event) => setDescription(event.target.value)} />
        </Field>
        <Field
          label="Memory budget (MiB, blank for default)"
          presets={
            <QuickPick
              label={t("Memory presets")}
              options={CACHE_MEMORY_MIB}
              value={memory}
              onPick={setMemory}
            />
          }
        >
          <TextInput
            inputMode="numeric"
            value={memory}
            onChange={(event) => setMemory(event.target.value)}
          />
        </Field>
        <Field
          label="Largest object (MiB, blank for 16)"
          presets={
            <QuickPick
              label={t("Object size presets")}
              options={CACHE_OBJECT_MIB}
              value={maxObject}
              onPick={setMaxObject}
            />
          }
        >
          <TextInput
            inputMode="numeric"
            value={maxObject}
            onChange={(event) => setMaxObject(event.target.value)}
          />
        </Field>
        <div className="md:col-span-2">
          <Field label="Rules (JSON, first match wins)">
            <TextArea
              className="min-h-72"
              value={rulesText}
              onChange={(event) => setRulesText(event.target.value)}
            />
          </Field>
          <p className="mt-1 text-xs text-muted">
            Each rule has <code>match</code> (path_prefixes, extensions, path_glob) and{" "}
            <code>action</code> (mode cache|bypass, ttl_secs, respect_origin,
            stale_while_revalidate_secs, stale_if_error_secs, ignore_query, query_keys,
            bypass_cookies, statuses). Responses with Set-Cookie, private, or no-store are never
            stored.
          </p>
        </div>
        <div className="flex gap-2 md:col-span-2">
          <Button type="submit">Save</Button>
          <Button quiet type="button" onClick={onDone}>
            Cancel
          </Button>
          {policy ? (
            <Button
              quiet
              type="button"
              className="ml-auto"
              onClick={() => {
                if (window.confirm(`Delete ${policy.name}? Its domains use the default policy.`)) {
                  remove.mutate();
                }
              }}
            >
              Delete
            </Button>
          ) : null}
        </div>
      </form>
    </Card>
  );
}

function CacheServers() {
  const servers = useQuery({
    queryKey: ["servers"],
    queryFn: () => api<Schemas["ServerView"][]>("/api/v1/servers"),
  });
  return (
    <Card title="Cache servers">
      <ul className="divide-y divide-line text-sm">
        {(servers.data ?? []).map((server) => (
          <ServerStats key={server.id} server={server} />
        ))}
      </ul>
      {servers.data?.length === 0 ? <Empty>No servers.</Empty> : null}
    </Card>
  );
}

function ServerStats({ server }: { server: Schemas["ServerView"] }) {
  const stats = useQuery({
    queryKey: ["cache-stats", server.id],
    queryFn: () => api<Schemas["CacheStats"]>(`/api/v1/servers/${server.id}/cache/stats`),
    refetchInterval: 15_000,
    retry: false,
  });
  const totals = (stats.data?.hosts ?? []).reduce(
    (sum, host) => ({
      hits: sum.hits + host.hits,
      misses: sum.misses + host.misses,
      stale: sum.stale + host.stale,
    }),
    { hits: 0, misses: 0, stale: 0 },
  );
  return (
    <li className="flex flex-wrap items-center gap-3 py-2">
      <span className="font-medium">{server.name}</span>
      {stats.data ? (
        <span className="text-muted">
          hit ratio {hitRatio(totals)} · {stats.data.objects} objects ·{" "}
          {formatBytes(stats.data.stored_bytes)} of {formatBytes(stats.data.memory_max_bytes)} ·{" "}
          {stats.data.evictions} evictions · rules v{stats.data.rules_version}
        </span>
      ) : (
        <span className="text-muted">
          {stats.isLoading ? "Checking…" : errorMessage(stats.error, "cache server unavailable")}
        </span>
      )}
    </li>
  );
}
