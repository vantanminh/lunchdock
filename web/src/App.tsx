import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";
import { Toaster } from "sonner";
import { AuthProvider, useAuth } from "./auth";
import { Shell } from "./layout/Shell";
import { AccountPage } from "./pages/AccountPage";
import { AuditPage } from "./pages/AuditPage";
import { DashboardPage } from "./pages/Dashboard";
import { CertificatesPage } from "./pages/CertificatesPage";
import { ProjectsPage } from "./pages/ProjectsPage";
import { RegistriesPage } from "./pages/RegistriesPage";
import { ServersPage } from "./pages/ServersPage";
import { ServicePage } from "./pages/ServicePage";
import { LoginPage, SetupPage } from "./pages/AuthPages";
import { TokensPage } from "./pages/TokensPage";
import { UsersPage } from "./pages/UsersPage";

function Guard({ children }: { children: ReactNode }) {
  const { user, loading, needsSetup } = useAuth();
  if (loading) {
    return <p className="p-6 text-sm text-muted">Loading Rudder…</p>;
  }
  if (needsSetup) {
    return <Navigate to="/setup" replace />;
  }
  if (!user) {
    return <Navigate to="/login" replace />;
  }
  return children;
}

function AppRoutes() {
  return (
    <Routes>
      <Route path="/setup" element={<SetupPage />} />
      <Route path="/login" element={<LoginPage />} />
      <Route
        element={
          <Guard>
            <Shell />
          </Guard>
        }
      >
        <Route index element={<DashboardPage />} />
        <Route path="servers" element={<ServersPage />} />
        <Route path="projects" element={<ProjectsPage />} />
        <Route path="services/:id" element={<ServicePage />} />
        <Route path="certificates" element={<CertificatesPage />} />
        <Route path="registries" element={<RegistriesPage />} />
        <Route path="users" element={<UsersPage />} />
        <Route path="tokens" element={<TokensPage />} />
        <Route path="audit" element={<AuditPage />} />
        <Route path="account" element={<AccountPage />} />
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}

export function App() {
  const [client] = useState(() => new QueryClient());
  return (
    <QueryClientProvider client={client}>
      <AuthProvider>
        <BrowserRouter>
          <AppRoutes />
        </BrowserRouter>
        <Toaster theme={document.documentElement.classList.contains("dark") ? "dark" : "light"} />
      </AuthProvider>
    </QueryClientProvider>
  );
}
