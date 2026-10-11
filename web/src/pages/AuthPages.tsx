import { useState, type FormEvent } from "react";
import { Navigate } from "react-router-dom";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { rememberSession, useAuth } from "../auth";
import { Button, Card, Field, TextInput } from "../components/ui";
import type { SessionResponse } from "../types";

export function SetupPage() {
  const { needsSetup, loading, refresh, user } = useAuth();
  const [email, setEmail] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);

  if (!loading && user) {
    return <Navigate to="/" replace />;
  }
  if (!loading && !needsSetup) {
    return <Navigate to="/login" replace />;
  }

  async function onSubmit(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    try {
      const body = await api<SessionResponse>("/api/v1/setup", {
        method: "POST",
        body: JSON.stringify({
          email,
          password,
          display_name: displayName || undefined,
        }),
      });
      rememberSession(body);
      await refresh();
    } catch (error) {
      toast.error(error instanceof ApiError ? error.message : "Setup failed");
    } finally {
      setPending(false);
    }
  }

  return (
    <AuthFrame title="Create the owner" subtitle="This account administers the panel.">
      <form className="space-y-4" onSubmit={(event) => void onSubmit(event)}>
        <Field label="Email">
          <TextInput
            type="email"
            required
            value={email}
            onChange={(event) => setEmail(event.target.value)}
          />
        </Field>
        <Field label="Display name">
          <TextInput value={displayName} onChange={(event) => setDisplayName(event.target.value)} />
        </Field>
        <Field label="Password">
          <TextInput
            type="password"
            required
            minLength={12}
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </Field>
        <Button type="submit" disabled={pending} className="w-full">
          {pending ? "Creating…" : "Create owner"}
        </Button>
      </form>
    </AuthFrame>
  );
}

export function LoginPage() {
  const { needsSetup, loading, refresh, user } = useAuth();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [totp, setTotp] = useState("");
  const [needsTotp, setNeedsTotp] = useState(false);
  const [pending, setPending] = useState(false);

  if (!loading && needsSetup) {
    return <Navigate to="/setup" replace />;
  }
  if (!loading && user) {
    return <Navigate to="/" replace />;
  }

  async function onSubmit(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    try {
      const body = await api<SessionResponse>("/api/v1/login", {
        method: "POST",
        body: JSON.stringify({
          email,
          password,
          totp_code: totp || undefined,
        }),
      });
      rememberSession(body);
      await refresh();
    } catch (error) {
      if (error instanceof ApiError && error.code === "totp_required") {
        setNeedsTotp(true);
        toast.message("Enter the authenticator code");
      } else {
        toast.error(error instanceof ApiError ? error.message : "Sign-in failed");
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <AuthFrame title="Sign in" subtitle="Rudder control plane">
      <form className="space-y-4" onSubmit={(event) => void onSubmit(event)}>
        <Field label="Email">
          <TextInput
            type="email"
            required
            value={email}
            onChange={(event) => setEmail(event.target.value)}
          />
        </Field>
        <Field label="Password">
          <TextInput
            type="password"
            required
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </Field>
        {needsTotp ? (
          <Field label="Authenticator code">
            <TextInput
              inputMode="numeric"
              autoComplete="one-time-code"
              value={totp}
              onChange={(event) => setTotp(event.target.value)}
            />
          </Field>
        ) : null}
        <Button type="submit" disabled={pending} className="w-full">
          {pending ? "Signing in…" : "Sign in"}
        </Button>
      </form>
    </AuthFrame>
  );
}

function AuthFrame({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <main className="mx-auto flex min-h-screen max-w-md flex-col justify-center px-4 py-10">
      <p className="text-xs font-semibold tracking-[0.18em] text-accent uppercase">Rudder</p>
      <h1 className="mt-2 text-3xl font-semibold">{title}</h1>
      <p className="mt-1 mb-6 text-sm text-muted">{subtitle}</p>
      <Card>{children}</Card>
    </main>
  );
}
