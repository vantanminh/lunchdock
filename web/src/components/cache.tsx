import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { api } from "../api";
import { errorMessage, formatBytes } from "../format";
import type { Schemas } from "../types";
import { Badge, Button, Card, Empty, Field, Select, TextArea } from "./ui";

type ServiceCache = Schemas["ServiceCacheView"];
type Policy = Schemas["CachePolicyView"];
type Rule = Schemas["CacheRule"];

export function usePolicies() {
  return useQuery({
    queryKey: ["cache-policies"],
    queryFn: () => api<Policy[]>("/api/v1/cache-policies"),
  });
}

export function hitRatio(stats: { hits: number; misses: number; stale: number }): string {
  const served = stats.hits + stats.stale;
  const total = served + stats.misses;
  return total === 0 ? "—" : `${Math.round((served / total) * 100)}%`;
}

export function ruleSummary(rule: Rule): string {
  const match: Partial<Schemas["RuleMatch"]> = rule.match ?? {};
  const parts: string[] = [];
  if (match.path_prefixes?.length) parts.push(`paths ${match.path_prefixes.join(", ")}`);
  if (match.extensions?.length) parts.push(`.${match.extensions.slice(0, 6).join(" .")}`);
  if (match.path_glob) parts.push(`glob ${match.path_glob}`);
  const scope = parts.length ? parts.join(" · ") : "everything";
  if (rule.action.mode === "bypass") return `${scope} → bypass`;
  const ttl = rule.action.respect_origin
    ? `origin headers, else ${rule.action.ttl_secs}s`
    : `${rule.action.ttl_secs}s`;
  return `${scope} → cache ${ttl}`;
}

export function ServiceCachePanel({ serviceId }: { serviceId: string }) {
  const queryClient = useQueryClient();
  const cache = useQuery({
    queryKey: ["service-cache", serviceId],
    queryFn: () => api<ServiceCache>(`/api/v1/services/${serviceId}/cache`),
    refetchInterval: 10_000,
  });
  const policies = usePolicies();
  const [scope, setScope] = useState("");
  const [paths, setPaths] = useState("");

  const toggle = useMutation({
    mutationFn: (input: { domainId: string; enabled: boolean; policyId: string | null }) =>
      api(`/api/v1/domains/${input.domainId}/cache`, {
        method: "PUT",
        body: JSON.stringify({ enabled: input.enabled, policy_id: input.policyId }),
      }),
    onSuccess: async () => {
      toast.success("Cache settings published");
      await queryClient.invalidateQueries({ queryKey: ["service-cache", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not update the domain")),
  });

  const purge = useMutation({
    mutationFn: (all: boolean) => {
      const list = paths
        .split("\n")
        .map((line) => line.trim())
        .filter(Boolean);
      const first = list.length === 1 ? list[0] : undefined;
      const prefix = first?.endsWith("*") ? first.slice(0, -1) : null;
      return api<Schemas["CachePurgeResult"]>(`/api/v1/services/${serviceId}/cache/purge`, {
        method: "POST",
        body: JSON.stringify({
          domain_id: scope || undefined,
          all: all || undefined,
          prefix: !all && prefix ? prefix : undefined,
          paths: !all && !prefix ? list : undefined,
        }),
      });
    },
    onSuccess: async (result) => {
      toast.success(`Purged ${result.removed} object${result.removed === 1 ? "" : "s"}`);
      setPaths("");
      await queryClient.invalidateQueries({ queryKey: ["service-cache", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, "Purge failed")),
  });

  const domains = cache.data?.domains ?? [];
  const cached = domains.filter((domain) => domain.enabled);

  return (
    <>
      <Card title="Cached domains">
        <p className="mb-3 text-sm text-muted">
          Cached domains go through the server&apos;s Rudder cache before reaching the container.
          Policies decide what is stored and for how long;{" "}
          <Link className="text-accent" to="/cache">
            manage policies
          </Link>
          .
        </p>
        {domains.length === 0 ? <Empty>Add a domain first.</Empty> : null}
        <ul className="divide-y divide-line text-sm">
          {domains.map((domain) => (
            <li key={domain.domain_id} className="flex flex-wrap items-center gap-3 py-3">
              <span className="font-medium">
                {domain.host}
                {domain.path_prefix !== "/" ? domain.path_prefix : ""}
              </span>
              <Badge value={domain.enabled ? "running" : "stopped"} />
              <div className="ml-auto flex items-center gap-2">
                <Select
                  aria-label={`Policy for ${domain.host}`}
                  className="w-56"
                  value={domain.policy_id ?? ""}
                  onChange={(event) =>
                    toggle.mutate({
                      domainId: domain.domain_id,
                      enabled: domain.enabled,
                      policyId: event.target.value || null,
                    })
                  }
                >
                  <option value="">Honor origin headers (default)</option>
                  {(policies.data ?? []).map((policy) => (
                    <option key={policy.id} value={policy.id}>
                      {policy.name}
                    </option>
                  ))}
                </Select>
                <Button
                  quiet={domain.enabled}
                  onClick={() =>
                    toggle.mutate({
                      domainId: domain.domain_id,
                      enabled: !domain.enabled,
                      policyId: domain.policy_id ?? null,
                    })
                  }
                >
                  {domain.enabled ? "Turn off" : "Turn on"}
                </Button>
              </div>
            </li>
          ))}
        </ul>
      </Card>
      {cached.length > 0 ? (
        <>
          <Card title="Hit ratio">
            {cache.data?.stats_error ? (
              <p className="text-sm text-muted">
                Stats unavailable: {cache.data.stats_error}. Requests still pass through once the
                cache server is up.
              </p>
            ) : null}
            <div className="overflow-x-auto">
              <table className="w-full text-left text-sm">
                <thead className="text-muted">
                  <tr>
                    <th className="py-1 pr-4 font-medium">Host</th>
                    <th className="py-1 pr-4 font-medium">Hit ratio</th>
                    <th className="py-1 pr-4 font-medium">Hits</th>
                    <th className="py-1 pr-4 font-medium">Misses</th>
                    <th className="py-1 pr-4 font-medium">Bypass</th>
                    <th className="py-1 pr-4 font-medium">Objects</th>
                    <th className="py-1 pr-4 font-medium">Stored</th>
                  </tr>
                </thead>
                <tbody>
                  {(cache.data?.stats ?? []).map((row) => (
                    <tr key={row.host} className="border-t border-line">
                      <td className="py-1 pr-4">{row.host}</td>
                      <td className="py-1 pr-4 font-medium">{hitRatio(row)}</td>
                      <td className="py-1 pr-4">{row.hits + row.stale}</td>
                      <td className="py-1 pr-4">{row.misses}</td>
                      <td className="py-1 pr-4">{row.bypass}</td>
                      <td className="py-1 pr-4">{row.objects}</td>
                      <td className="py-1 pr-4">{formatBytes(row.stored_bytes)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </Card>
          <Card title="Purge">
            <form
              className="grid gap-3"
              onSubmit={(event) => {
                event.preventDefault();
                purge.mutate(false);
              }}
            >
              <Field label="Domain">
                <Select value={scope} onChange={(event) => setScope(event.target.value)}>
                  <option value="">All cached domains</option>
                  {cached.map((domain) => (
                    <option key={domain.domain_id} value={domain.domain_id}>
                      {domain.host}
                      {domain.path_prefix !== "/" ? domain.path_prefix : ""}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label="Paths (one per line, or a single /prefix*)">
                <TextArea
                  value={paths}
                  placeholder={"/index.html\n/blog/?page=2"}
                  onChange={(event) => setPaths(event.target.value)}
                />
              </Field>
              <div className="flex gap-2">
                <Button type="submit" disabled={!paths.trim()}>
                  Purge paths
                </Button>
                <Button
                  quiet
                  type="button"
                  onClick={() => {
                    if (window.confirm("Remove every cached object for these domains?")) {
                      purge.mutate(true);
                    }
                  }}
                >
                  Purge everything
                </Button>
              </div>
            </form>
          </Card>
        </>
      ) : null}
    </>
  );
}
