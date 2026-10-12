import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Alert, buttonClass, Card, PageHeader, StatusDot } from "../components/ui";
import { errorMessage } from "../format";
import { useT } from "../i18n";
import type { Schemas } from "../types";

type Created = Schemas["GithubAppCreated"];
type GitProvider = Schemas["GitProviderView"];

/**
 * Where GitHub sends the browser back to, twice: after it creates the app
 * (`code` and `state`) and after the app is installed (`installation_id`).
 * Each step is one API call; the page only reports progress.
 */
export function GitHubCallbackPage() {
  const t = useT();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [params] = useSearchParams();
  const code = params.get("code");
  const state = params.get("state");
  const installationId = params.get("installation_id");
  const waitingForApproval = params.get("setup_action") === "request";
  const [error, setError] = useState<string | null>(null);
  // Effects run twice in development; a one-time code must not be sent twice.
  const started = useRef(false);

  const step = code && state ? "create" : installationId ? "install" : "other";

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    const refresh = () => queryClient.invalidateQueries({ queryKey: ["git-providers"] });

    async function run() {
      try {
        if (step === "create") {
          const created = await api<Created>("/api/v1/github-app/complete", {
            method: "POST",
            body: JSON.stringify({ code, state }),
          });
          await refresh();
          // Carry on to GitHub to pick the repositories.
          window.location.replace(created.install_url);
        } else if (step === "install") {
          // Without `state` this is GitHub reporting a change made in its own settings,
          // or an install that did not start here; the panel looks for a pending app.
          const provider = await api<GitProvider>("/api/v1/github-app/installed", {
            method: "POST",
            body: JSON.stringify({ installation_id: installationId, state: state ?? undefined }),
          }).catch((caught: unknown) => {
            if (!state && caught instanceof ApiError && caught.code === "no_pending_app") {
              return null;
            }
            throw caught;
          });
          await refresh();
          toast.success(
            provider?.account
              ? t("GitHub connected as {account}", { account: provider.account })
              : provider
                ? t("GitHub connected")
                : t("Repository access updated"),
          );
          navigate("/git", { replace: true });
        } else if (!waitingForApproval) {
          navigate("/git", { replace: true });
        }
      } catch (caught) {
        if (caught instanceof ApiError && caught.code === "already_connected") {
          navigate("/git", { replace: true });
          return;
        }
        setError(errorMessage(caught, t("Something went wrong talking to GitHub.")));
      }
    }
    void run();
  }, [code, installationId, navigate, queryClient, state, step, t, waitingForApproval]);

  const back = (
    <Link to="/git" className={buttonClass({ variant: "secondary" })}>
      {t("Back to Git providers")}
    </Link>
  );

  return (
    <>
      <PageHeader title={t("Connect GitHub")} />
      {error ? (
        <Card>
          <div className="grid gap-4">
            <Alert tone="error" title={t("GitHub setup did not finish")}>
              {error}
            </Alert>
            <div>{back}</div>
          </div>
        </Card>
      ) : waitingForApproval ? (
        <Card>
          <div className="grid gap-4">
            <Alert tone="info" title={t("Waiting for approval")}>
              {t(
                "An owner of the GitHub organization has to approve the installation. Once they do, open Manage repositories to finish.",
              )}
            </Alert>
            <div>{back}</div>
          </div>
        </Card>
      ) : (
        <Card>
          <p className="flex items-center gap-3 text-sm text-ink-soft" role="status">
            <StatusDot status="building" />
            {step === "create"
              ? t("Creating your GitHub App…")
              : step === "install"
                ? t("Finishing the installation…")
                : t("Returning to Git providers…")}
          </p>
        </Card>
      )}
    </>
  );
}
