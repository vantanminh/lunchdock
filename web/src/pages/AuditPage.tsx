import { useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { useAuth } from "../auth";
import { Card } from "../components/ui";
import type { AuditEvent } from "../types";
import { formatTime } from "../format";
import { useT } from "../i18n";

export function AuditPage() {
  const t = useT();
  const { user } = useAuth();
  const audit = useQuery({
    queryKey: ["audit"],
    queryFn: () => api<AuditEvent[]>("/api/v1/audit"),
    enabled: user?.role === "admin",
  });

  if (user?.role !== "admin") {
    return <p className="text-sm text-muted">{t("Only admins can read the audit log.")}</p>;
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">{t("Audit log")}</h1>
      <Card>
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead className="text-muted">
              <tr>
                <th className="py-2 pr-3 font-medium">{t("When")}</th>
                <th className="py-2 pr-3 font-medium">{t("Action")}</th>
                <th className="py-2 pr-3 font-medium">{t("Resource")}</th>
                <th className="py-2 font-medium">{t("IP")}</th>
              </tr>
            </thead>
            <tbody>
              {(audit.data ?? []).map((event) => (
                <tr key={event.id} className="border-t border-line">
                  <td className="py-2 pr-3 whitespace-nowrap">{formatTime(event.created_at)}</td>
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
