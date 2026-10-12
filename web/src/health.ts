import { toneOf } from "./components/ui";

/** Failed first, then work in flight, then healthy, then idle. */
export const statusOrder: Record<string, number> = {
  failed: 0,
  error: 0,
  partial: 1,
  rolled_back: 1,
  deploying: 2,
  building: 2,
  queued: 2,
  running: 3,
  stopped: 4,
};

/** A failed or degraded service: it needs somebody to look at it. */
export function isIssue(status: string): boolean {
  const tone = toneOf(status);
  return tone === "danger" || tone === "warning";
}

export interface HealthCounts {
  total: number;
  running: number;
  issues: number;
  stopped: number;
  /** Everything that is neither running nor in trouble: stopped, queued, deploying. */
  other: number;
}

export function summarize(services: readonly { status: string }[]): HealthCounts {
  let running = 0;
  let issues = 0;
  let stopped = 0;
  for (const service of services) {
    if (service.status === "running") running += 1;
    else if (isIssue(service.status)) issues += 1;
    else if (service.status === "stopped") stopped += 1;
  }
  return {
    total: services.length,
    running,
    issues,
    stopped,
    other: services.length - running - issues,
  };
}
