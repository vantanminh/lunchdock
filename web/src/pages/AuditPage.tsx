import { useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { useAuth } from "../auth";
import { Card } from "../components/ui";
import type { AuditEvent } from "../types";

export function AuditPage() {
  const { user } = useAuth();
  const audit = useQuery({
    queryKey: ["audit"],
    queryFn: () => api<AuditEvent[]>("/api/v1/audit"),
    enabled: user?.role === "admin",
  });

  if (user?.role !== "admin") {
    return <p className="text-sm text-muted">Only admins can read the audit log.</p>;
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">Audit log</h1>
      <Card>
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead className="text-muted">
              <tr>
                <th className="py-2 pr-3 font-medium">When</th>
                <th className="py-2 pr-3 font-medium">Action</th>
                <th className="py-2 pr-3 font-medium">Resource</th>
                <th className="py-2 font-medium">IP</th>
              </tr>
            </thead>
            <tbody>
              {(audit.data ?? []).map((event) => (
                <tr key={event.id} className="border-t border-line">
                  <td className="py-2 pr-3 whitespace-nowrap">
                    {new Date(event.created_at).toLocaleString()}
                  </td>
                  <td className="py-2 pr-3">{event.action}</td>
                  <td className="py-2 pr-3">
                    {event.resource_type}
                    {event.resource_id ? ` ${event.resource_id.slice(0, 8)}` : ""}
                  </td>
                  <td className="py-2">{event.ip ?? ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Card>
    </>
  );
}
