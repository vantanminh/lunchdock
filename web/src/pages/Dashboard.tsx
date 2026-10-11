import { useAuth } from "../auth";
import { Card } from "../components/ui";

export function DashboardPage() {
  const { user } = useAuth();
  return (
    <>
      <h1 className="text-2xl font-semibold">Dashboard</h1>
      <Card title="Panel">
        <p className="text-sm text-muted">
          Signed in as {user?.email} ({user?.role}). Servers, projects, and deploys attach to this
          dashboard as each milestone lands.
        </p>
      </Card>
    </>
  );
}
