import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { useAuth } from "../auth";
import { LOG_SOURCES, LogViewer } from "../components/logs";
import { Button, Card, Field, TextInput } from "../components/ui";
import { errorMessage } from "../format";
import type { Schemas } from "../types";

type Retention = Schemas["RetentionSettings"];

export function LogsPage() {
  const { user } = useAuth();
  return (
    <>
      <h1 className="text-2xl font-semibold">Logs</h1>
      <p className="text-sm text-muted">
        Application, proxy, cache, and panel logs from every server, with live tail and export.
      </p>
      <Card>
        <LogViewer />
      </Card>
      {user?.role === "admin" ? <RetentionCard /> : null}
    </>
  );
}

function RetentionCard() {
  const queryClient = useQueryClient();
  const retention = useQuery({
    queryKey: ["retention"],
    queryFn: () => api<Retention>("/api/v1/settings/retention"),
  });
  const [days, setDays] = useState<Record<string, string>>({});
  useEffect(() => {
    if (retention.data) {
      setDays(
        Object.fromEntries(
          Object.entries(retention.data.log_days).map(([source, value]) => [source, String(value)]),
        ),
      );
    }
  }, [retention.data]);
  const save = useMutation({
    mutationFn: () =>
      api<Retention>("/api/v1/settings/retention", {
        method: "PUT",
        body: JSON.stringify({
          log_days: Object.fromEntries(
            Object.entries(days).map(([source, value]) => [source, Number(value)]),
          ),
        }),
      }),
    onSuccess: async () => {
      toast.success("Retention saved");
      await queryClient.invalidateQueries({ queryKey: ["retention"] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save retention")),
  });

  return (
    <Card title="Retention">
      <p className="mb-3 text-sm text-muted">
        Days to keep per source (1–365). Expired lines are removed hourly; whole daily partitions
        are dropped once every source has expired them.
      </p>
      <form
        className="grid gap-3 md:grid-cols-6"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        {LOG_SOURCES.map((source) => (
          <Field key={source} label={source}>
            <TextInput
              type="number"
              min={1}
              max={365}
              value={days[source] ?? ""}
              onChange={(event) => setDays({ ...days, [source]: event.target.value })}
            />
          </Field>
        ))}
        <div className="md:col-span-6">
          <Button type="submit" disabled={save.isPending || !retention.data}>
            Save retention
          </Button>
        </div>
      </form>
    </Card>
  );
}
