import { useEffect, useState, type FormEvent } from "react";
import { useSearchParams } from "react-router-dom";
import { ApiError } from "../api";
import { Turnstile } from "../components/turnstile";
import { Button, Card, Field, TextInput } from "../components/ui";
import { useT } from "../i18n";
import type { Schemas } from "../types";

type PublicConfig = Schemas["AccessPublic"];
type Me = Schemas["AccessMe"];
type AppItem = Schemas["AccessApp"];

async function accessFetch<T>(path: string, init: RequestInit = {}, csrf?: string): Promise<T> {
  const headers = new Headers(init.headers);
  const method = (init.method ?? "GET").toUpperCase();
  if (init.body && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  if (csrf && method !== "GET" && method !== "HEAD") {
    headers.set("x-csrf-token", csrf);
  }
  const response = await fetch(path, { ...init, headers, credentials: "same-origin" });
  const text = await response.text();
  const body = text ? (JSON.parse(text) as T & { error?: { code?: string; message?: string } }) : ({} as T);
  if (!response.ok) {
    const error = body as { error?: { code?: string; message?: string } };
    throw new ApiError(
      response.status,
      error.error?.code ?? "error",
      error.error?.message ?? response.statusText,
    );
  }
  return body as T;
}

export function AccessPortalPage() {
  const t = useT();
  const [params] = useSearchParams();
  const [config, setConfig] = useState<PublicConfig | null>(null);
  const [me, setMe] = useState<Me | null>(null);
  const [apps, setApps] = useState<AppItem[] | null>(null);
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [sent, setSent] = useState(false);
  const [token, setToken] = useState("");
  const [widgetKey, setWidgetKey] = useState(0);
  const [message, setMessage] = useState("");
  const [pending, setPending] = useState(false);

  const returnHost = params.get("host") ?? "";
  const returnPath = params.get("path") ?? "/";
  const blocked = params.get("error") === "blocked";

  async function loadSession() {
    const current = await accessFetch<Me>("/api/v1/access/me");
    setMe(current);
    const list = await accessFetch<AppItem[]>("/api/v1/access/apps");
    setApps(list);
  }

  useEffect(() => {
    void accessFetch<PublicConfig>("/api/v1/access/public")
      .then(setConfig)
      .catch(() => setMessage(t("Rudder Access is not ready")));
    void loadSession().catch(() => setMe(null));
    // Load once when the portal opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function resetWidget() {
    setToken("");
    setWidgetKey((value) => value + 1);
  }

  async function sendCode(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    setMessage("");
    try {
      await accessFetch("/api/v1/access/login/email", {
        method: "POST",
        body: JSON.stringify({
          email,
          turnstile_token: token,
          return_host: returnHost || undefined,
          return_path: returnPath || undefined,
        }),
      });
      setSent(true);
      setMessage(t("Check your email for a 6-digit code."));
    } catch (error) {
      setMessage(error instanceof ApiError ? error.message : t("Could not send the code"));
    } finally {
      setPending(false);
      resetWidget();
    }
  }

  async function verify(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    setMessage("");
    try {
      const result = await accessFetch<Schemas["AccessLoginResult"]>("/api/v1/access/login/email/verify", {
        method: "POST",
        body: JSON.stringify({
          email,
          code,
          turnstile_token: token,
          return_host: returnHost || undefined,
          return_path: returnPath || undefined,
        }),
      });
      if (result.redirect) {
        window.location.assign(result.redirect);
        return;
      }
      await loadSession();
    } catch (error) {
      setMessage(error instanceof ApiError ? error.message : t("Could not sign in"));
    } finally {
      setPending(false);
      resetWidget();
    }
  }

  async function google() {
    setPending(true);
    setMessage("");
    try {
      const result = await accessFetch<Schemas["AccessGoogleUrl"]>("/api/v1/access/login/google", {
        method: "POST",
        body: JSON.stringify({
          turnstile_token: token,
          return_host: returnHost || undefined,
          return_path: returnPath || undefined,
        }),
      });
      window.location.assign(result.url);
    } catch (error) {
      setMessage(error instanceof ApiError ? error.message : t("Could not start Google sign-in"));
      setPending(false);
      resetWidget();
    }
  }

  async function logout() {
    await accessFetch("/api/v1/access/logout", { method: "POST" }, me?.csrf_token);
    setMe(null);
    setApps(null);
  }

  return (
    <main className="mx-auto grid min-h-screen max-w-lg gap-6 px-4 py-10">
      <div>
        <h1 className="text-2xl font-semibold">{t("Rudder Access")}</h1>
        <p className="text-sm text-muted">
          {t("Sign in to open the internal apps your organization has allowed.")}
        </p>
      </div>
      {blocked ? (
        <Card title={t("Access blocked")}>
          <p className="text-sm text-muted">
            {t("This email is not allowed to sign in. Ask an administrator to add it.")}
          </p>
        </Card>
      ) : null}
      {me && apps ? (
        <Card
          title={t("Your apps")}
          description={t("Signed in as {email}", { email: me.email })}
          actions={
            <Button quiet onClick={() => void logout()}>
              {t("Sign out")}
            </Button>
          }
        >
          {apps.length === 0 ? (
            <p className="text-sm text-muted">{t("No internal apps are assigned to you yet.")}</p>
          ) : (
            <ul className="divide-y divide-line">
              {apps.map((app) => (
                <li key={app.service_id} className="flex items-center justify-between py-3">
                  <span className="font-medium">{app.name}</span>
                  {app.host ? (
                    <a className="text-sm text-pine" href={`https://${app.host}`}>
                      {app.host}
                    </a>
                  ) : (
                    <span className="text-sm text-muted">{t("No domain yet")}</span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </Card>
      ) : (
        <Card title={t("Sign in")}>
          {!config?.ready ? (
            <p className="text-sm text-muted">
              {t("An administrator still needs to turn on Turnstile and an email provider.")}
            </p>
          ) : (
            <form className="grid gap-4" onSubmit={(event) => void (sent ? verify(event) : sendCode(event))}>
              <Field label={t("Email")}>
                <TextInput
                  type="email"
                  required
                  value={email}
                  onChange={(event) => setEmail(event.target.value)}
                />
              </Field>
              {sent ? (
                <Field label={t("6-digit code")}>
                  <TextInput
                    required
                    inputMode="numeric"
                    pattern="[0-9]{6}"
                    value={code}
                    onChange={(event) => setCode(event.target.value)}
                  />
                </Field>
              ) : null}
              <Turnstile key={widgetKey} siteKey={config.site_key} action="access_login" onToken={setToken} />
              <Button type="submit" disabled={pending || !token}>
                {sent ? t("Sign in") : t("Email me a code")}
              </Button>
              {config.google ? (
                <Button type="button" variant="secondary" disabled={pending || !token} onClick={() => void google()}>
                  {t("Continue with Google")}
                </Button>
              ) : null}
            </form>
          )}
          {message ? <p className="mt-3 text-sm text-muted">{message}</p> : null}
        </Card>
      )}
    </main>
  );
}
