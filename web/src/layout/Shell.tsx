import { Suspense, useState } from "react";
import { NavLink, Outlet } from "react-router-dom";
import { api, setCsrf } from "../api";
import { useAuth } from "../auth";
import { Button } from "../components/ui";

const links = [
  ["/", "Dashboard"],
  ["/servers", "Servers"],
  ["/projects", "Projects"],
  ["/templates", "Templates"],
  ["/monitoring", "Monitoring"],
  ["/alerts", "Alerts"],
  ["/logs", "Logs"],
  ["/backups", "Backups"],
  ["/s3-destinations", "S3 destinations"],
  ["/cache", "Cache"],
  ["/certificates", "Certificates"],
  ["/registries", "Registries"],
  ["/git", "Git providers"],
  ["/users", "Users"],
  ["/tokens", "API tokens"],
  ["/audit", "Audit log"],
  ["/system", "System"],
  ["/account", "Account"],
] as const;

export function Shell() {
  const { user, refresh } = useAuth();

  async function logout() {
    await api("/api/v1/logout", { method: "POST" });
    setCsrf(null);
    await refresh();
  }

  return (
    <div className="min-h-screen md:grid md:grid-cols-[220px_1fr]">
      <aside className="border-b border-line bg-panel md:border-r md:border-b-0">
        <div className="flex items-center justify-between px-4 py-4 md:block">
          <div>
            <p className="text-xs font-semibold tracking-[0.18em] text-accent uppercase">Rudder</p>
            <p className="mt-1 text-sm text-muted">{user?.display_name}</p>
          </div>
          <Button quiet className="md:mt-4 md:w-full" onClick={() => void logout()}>
            Sign out
          </Button>
        </div>
        <nav className="flex gap-1 overflow-x-auto px-2 pb-3 md:block md:px-2">
          {links.map(([to, label]) => (
            <NavLink
              key={to}
              to={to}
              end={to === "/"}
              className={({ isActive }) =>
                `block rounded-md px-3 py-2 text-sm whitespace-nowrap ${isActive ? "bg-paper font-medium text-ink" : "text-muted"}`
              }
            >
              {label}
            </NavLink>
          ))}
        </nav>
      </aside>
      <div className="min-w-0">
        <header className="flex justify-end border-b border-line px-4 py-3">
          <ThemeToggle />
        </header>
        <main className="mx-auto max-w-5xl space-y-4 px-4 py-6">
          <Suspense fallback={<p className="text-sm text-muted">Loading…</p>}>
            <Outlet />
          </Suspense>
        </main>
      </div>
    </div>
  );
}

function ThemeToggle() {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  return (
    <Button
      quiet
      onClick={() => {
        const next = !dark;
        document.documentElement.classList.toggle("dark", next);
        localStorage.setItem("rudder_theme", next ? "dark" : "light");
        setDark(next);
      }}
    >
      {dark ? "Light mode" : "Dark mode"}
    </Button>
  );
}
