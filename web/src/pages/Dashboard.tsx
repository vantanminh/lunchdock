import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { api } from "../api";
import { useAuth } from "../auth";
import { Badge, Card, Empty } from "../components/ui";
import type { Schemas } from "../types";

type Server = Schemas["ServerView"];
type Service = Schemas["ServiceSummary"];

export function DashboardPage() {
  const { user } = useAuth();
  const servers = useQuery({
    queryKey: ["servers"],
    queryFn: () => api<Server[]>("/api/v1/servers"),
  });
  const services = useQuery({
    queryKey: ["services"],
    queryFn: () => api<Service[]>("/api/v1/services"),
  });
  const serverList = servers.data ?? [];
  const serviceList = services.data ?? [];
  const online = serverList.filter((server) => server.status === "online").length;
  const running = serviceList.filter((service) => service.status === "running").length;
  const projects = new Set(serviceList.map((service) => service.project_id)).size;
  const serverName = new Map(serverList.map((server) => [server.id, server.name]));

  return (
    <>
      <h1 className="text-2xl font-semibold">Dashboard</h1>
      <p className="text-sm text-muted">
        Signed in as {user?.email} ({user?.role}).
      </p>
      <div className="grid gap-4 sm:grid-cols-3">
        <Stat label="Servers online" value={`${online} / ${serverList.length}`} to="/servers" />
        <Stat
          label="Services running"
          value={`${running} / ${serviceList.length}`}
          to="/projects"
        />
        <Stat label="Projects with services" value={String(projects)} to="/projects" />
      </div>
      <Card title="Services">
        {serviceList.length === 0 ? (
          <Empty>
            No services yet.{" "}
            <Link className="text-accent" to="/projects">
              Create a project
            </Link>{" "}
            or deploy a{" "}
            <Link className="text-accent" to="/templates">
              template
            </Link>
            .
          </Empty>
        ) : (
          <ul className="divide-y divide-line">
            {serviceList.map((service) => (
              <li key={service.id} className="flex items-center justify-between gap-3 py-2">
                <Link className="font-medium hover:text-accent" to={`/services/${service.id}`}>
                  {service.project_name}/{service.environment_name}/{service.name}
                </Link>
                <span className="flex items-center gap-3 text-sm text-muted">
                  {serverName.get(service.server_id) ?? "unknown server"}
                  <Badge value={service.status} />
                </span>
              </li>
            ))}
          </ul>
        )}
      </Card>
    </>
  );
}

function Stat({ label, value, to }: { label: string; value: string; to: string }) {
  return (
    <Link to={to} className="rounded-xl border border-line bg-panel p-4 hover:border-accent">
      <div className="text-sm text-muted">{label}</div>
      <div className="mt-1 text-2xl font-semibold">{value}</div>
    </Link>
  );
}
