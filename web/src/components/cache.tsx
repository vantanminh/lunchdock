import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { api } from "../api";
import { errorMessage, formatBytes } from "../format";
import { useT } from "../i18n";
import { PURGE_PATHS } from "../presets";
import type { Schemas } from "../types";
import {
  Alert,
  Badge,
  Button,
  Card,
  DataTable,
  Empty,
  Field,
  Meter,
  QuickPick,
  Select,
  Td,
  TextArea,
  Th,
} from "./ui";

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

/** The share of requests answered from cache, 0-100, or null before any traffic. */
function hitPercent(stats: { hits: number; misses: number; stale: number }): number | null {
  const served = stats.hits + stats.stale;
  const total = served + stats.misses;
  return total === 0 ? null : (served / total) * 100;
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
  const t = useT();
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
      toast.success(t("Cache settings published"));
      await queryClient.invalidateQueries({ queryKey: ["service-cache", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not update the domain"))),
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
      toast.success(
        result.removed === 1
          ? t("Purged {count} object", { count: result.removed })
          : t("Purged {count} objects", { count: result.removed }),
      );
      setPaths("");
      await queryClient.invalidateQueries({ queryKey: ["service-cache", serviceId] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Purge failed"))),
  });

  const domains = cache.data?.domains ?? [];
  const cached = domains.filter((domain) => domain.enabled);

  return (
    <>
      <Card
        title={t("Cached domains")}
        description={t(
          "Cached domains go through the server's Rudder cache before reaching the container. Policies decide what is stored and for how long; {link}.",
          {
            link: (
              <Link className="text-accent" to="/cache">
                {t("manage policies")}
              </Link>
            ),
          },
        )}
        flush
      >
        {domains.length === 0 ? (
          <Empty icon="globe" title={t("Nothing to cache yet")}>
            {t("Add a domain first.")}
          </Empty>
        ) : (
          <ul className="divide-y divide-line border-t border-line">
            {domains.map((domain) => (
              <li
                key={domain.domain_id}
                className="flex flex-wrap items-center gap-x-4 gap-y-3 px-5 py-3.5 sm:px-6"
              >
                <div className="flex min-w-0 flex-1 basis-52 items-center gap-3">
                  <span className="truncate text-sm font-medium text-ink">
                    {domain.host}
                    {domain.path_prefix !== "/" ? domain.path_prefix : ""}
                  </span>
                  <Badge value={domain.enabled ? "active" : "disabled"} />
                </div>
                <div className="w-72 max-w-full">
                  <Select
                    aria-label={t("Policy for {host}", { host: domain.host })}
                    value={domain.policy_id ?? ""}
                    onChange={(event) =>
                      toggle.mutate({
                        domainId: domain.domain_id,
                        enabled: domain.enabled,
                        policyId: event.target.value || null,
                      })
                    }
                  >
                    <option value="">{t("Honor origin headers (default)")}</option>
                    {(policies.data ?? []).map((policy) => (
                      <option key={policy.id} value={policy.id}>
                        {policy.name}
                      </option>
                    ))}
                  </Select>
                </div>
                <Button
                  variant={domain.enabled ? "secondary" : "primary"}
                  size="sm"
                  className="min-w-20"
                  disabled={toggle.isPending}
                  onClick={() =>
                    toggle.mutate({
                      domainId: domain.domain_id,
                      enabled: !domain.enabled,
                      policyId: domain.policy_id ?? null,
                    })
                  }
                >
                  {domain.enabled ? t("Turn off") : t("Turn on")}
                </Button>
              </li>
            ))}
          </ul>
        )}
      </Card>
      {cached.length > 0 ? (
        <>
          <Card
            title={t("Hit ratio")}
            description={t("How often requests are answered from the cache. Refreshes every 10 seconds.")}
            flush
          >
            {cache.data?.stats_error ? (
              <div className="px-5 pb-4 sm:px-6">
                <Alert tone="warning">
                  {t(
                    "Stats unavailable: {error}. Requests still pass through once the cache server is up.",
                    { error: cache.data.stats_error },
                  )}
                </Alert>
              </div>
            ) : null}
            <DataTable
              minWidth={720}
              head={
                <>
                  <Th>{t("Host")}</Th>
                  <Th className="w-44">{t("Hit ratio")}</Th>
                  <Th>{t("Hits")}</Th>
                  <Th>{t("Misses")}</Th>
                  <Th>{t("Bypass")}</Th>
                  <Th>{t("Objects")}</Th>
                  <Th>{t("Stored")}</Th>
                </>
              }
            >
              {(cache.data?.stats ?? []).map((row) => (
                <tr key={row.host} className="tabular-nums">
                  <Td className="font-medium text-ink">{row.host}</Td>
                  <Td>
                    <div className="flex items-center gap-2.5">
                      <span className="w-10 font-medium text-ink">{hitRatio(row)}</span>
                      <Meter
                        value={hitPercent(row)}
                        label={t("Hit ratio")}
                        className="w-24 [&>div]:bg-pine!"
                      />
                    </div>
                  </Td>
                  <Td>{(row.hits + row.stale).toLocaleString()}</Td>
                  <Td>{row.misses.toLocaleString()}</Td>
                  <Td>{row.bypass.toLocaleString()}</Td>
                  <Td>{row.objects.toLocaleString()}</Td>
                  <Td>{formatBytes(row.stored_bytes)}</Td>
                </tr>
              ))}
            </DataTable>
          </Card>
          <Card
            title={t("Purge")}
            description={t("Remove cached objects so visitors get fresh content on the next request.")}
          >
            <form
              className="grid gap-4"
              onSubmit={(event) => {
                event.preventDefault();
                purge.mutate(false);
              }}
            >
              <Field label={t("Domain")}>
                <Select value={scope} onChange={(event) => setScope(event.target.value)}>
                  <option value="">{t("All cached domains")}</option>
                  {cached.map((domain) => (
                    <option key={domain.domain_id} value={domain.domain_id}>
                      {domain.host}
                      {domain.path_prefix !== "/" ? domain.path_prefix : ""}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field
                label={t("Paths (one per line, or a single /prefix*)")}
                presets={
                  <QuickPick
                    label={t("Common paths")}
                    options={PURGE_PATHS}
                    onPick={(path) =>
                      setPaths(
                        path.endsWith("*")
                          ? path
                          : [...new Set([...paths.split("\n").filter(Boolean), path])].join("\n"),
                      )
                    }
                  />
                }
              >
                <TextArea
                  value={paths}
                  placeholder={"/index.html\n/blog/?page=2"}
                  onChange={(event) => setPaths(event.target.value)}
                />
              </Field>
              <div className="flex flex-wrap gap-2">
                <Button type="submit" icon="trash" disabled={!paths.trim() || purge.isPending}>
                  {t("Purge paths")}
                </Button>
                <Button
                  variant="danger"
                  type="button"
                  disabled={purge.isPending}
                  onClick={() => {
                    if (window.confirm(t("Remove every cached object for these domains?"))) {
                      purge.mutate(true);
                    }
                  }}
                >
                  {t("Purge everything")}
                </Button>
              </div>
            </form>
          </Card>
        </>
      ) : null}
    </>
  );
}
