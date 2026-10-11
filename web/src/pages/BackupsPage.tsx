import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { api } from "../api";
import { BackupJobForm, BackupJobs } from "../components/backups";
import { Badge, Card, Empty } from "../components/ui";
import { formatTime } from "../format";
import type { Schemas } from "../types";

export function BackupsPage() {
  const events = useQuery({
    queryKey: ["events"],
    queryFn: () => api<Schemas["EventView"][]>("/api/v1/events?limit=20"),
    refetchInterval: 10000,
  });
  return (
    <>
      <h1 className="text-2xl font-semibold">Backups</h1>
      <p className="text-sm text-muted">
        Database dumps and volume, bind, or host-path archives streamed to{" "}
        <Link className="text-accent" to="/s3-destinations">
          S3 destinations
        </Link>
        . Database backups are easiest to create from the database's Backups tab.
      </p>
      <Card title="Jobs">
        <BackupJobs />
      </Card>
      <Card title="New archive job">
        <BackupJobForm target={{}} />
      </Card>
      <Card title="Recent events">
        <ul className="divide-y divide-line text-sm">
          {(events.data ?? []).map((event) => (
            <li key={event.id} className="flex flex-wrap items-center gap-2 py-2">
              <Badge
                value={
                  event.severity === "critical"
                    ? "failed"
                    : event.severity === "warning"
                      ? "partial"
                      : "ok"
                }
              />
              <span className="font-medium">{event.title}</span>
              <span className="text-muted">{event.message}</span>
              <span className="ml-auto text-xs text-muted">{formatTime(event.created_at)}</span>
            </li>
          ))}
        </ul>
        {events.data?.length === 0 ? <Empty>No events yet.</Empty> : null}
      </Card>
    </>
  );
}
