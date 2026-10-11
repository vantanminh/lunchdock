import { useMutation, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { api, download } from "../api";
import { BackupJobForm, PanelRestoreHint } from "../components/backups";
import { Button, Card, Field, TextInput } from "../components/ui";
import { UpdatesCard } from "../components/updates";
import { errorMessage } from "../format";
import type { Schemas } from "../types";
import { useT } from "../i18n";

export function SystemPage() {
  const t = useT();
  const info = useQuery({
    queryKey: ["system"],
    queryFn: () => api<Schemas["SystemInfo"]>("/api/v1/system"),
  });
  const [passphrase, setPassphrase] = useState("");
  const [confirm, setConfirm] = useState("");
  const [includeHistory, setIncludeHistory] = useState(false);

  const backup = useMutation({
    mutationFn: () =>
      download("/api/v1/system/backup", {
        method: "POST",
        body: JSON.stringify({ passphrase, include_history: includeHistory }),
      }),
    onSuccess: () => {
      toast.success(t("Panel backup downloaded; keep the passphrase with it"));
      setPassphrase("");
      setConfirm("");
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not create the panel backup"))),
  });
  const exportSettings = useMutation({
    mutationFn: () => download("/api/v1/system/settings-export"),
    onError: (error) => toast.error(errorMessage(error, t("Could not export settings"))),
  });

  const mismatch = confirm.length > 0 && confirm !== passphrase;

  return (
    <>
      <h1 className="text-2xl font-semibold">{t("System")}</h1>
      <Card title={t("Version")}>
        {info.data ? (
          <dl className="grid gap-2 text-sm sm:grid-cols-[180px_1fr]">
            <dt className="text-muted">Rudder</dt>
            <dd>{info.data.version}</dd>
            <dt className="text-muted">{t("Database schema")}</dt>
            <dd>
              {info.data.schema}
              {info.data.schema === info.data.known_schema
                ? ` (${t("current")})`
                : ` (${t("binary knows {version}", { version: info.data.known_schema })})`}
            </dd>
            <dt className="text-muted">pg_dump</dt>
            <dd>{info.data.pg_dump ?? t("not available — panel backups cannot run")}</dd>
          </dl>
        ) : (
          <p className="text-sm text-muted">{t("Loading…")}</p>
        )}
      </Card>
      <UpdatesCard />
      <Card title={t("Panel backup")}>
        <p className="mb-3 text-sm text-muted">
          {t(
            "A complete copy of the panel: the database (projects, services, domains, users, backup jobs, encrypted secrets) and the master key that decrypts it. It is encrypted with the passphrase you choose here; Rudder does not store it.",
          )}
        </p>
        <form
          className="space-y-3"
          onSubmit={(event) => {
            event.preventDefault();
            if (!mismatch) {
              backup.mutate();
            }
          }}
        >
          <div className="grid gap-3 md:grid-cols-2">
            <Field label={t("Passphrase (12+ characters)")}>
              <TextInput
                type="password"
                autoComplete="new-password"
                required
                minLength={12}
                value={passphrase}
                onChange={(event) => setPassphrase(event.target.value)}
              />
            </Field>
            <Field label={t("Repeat passphrase")}>
              <TextInput
                type="password"
                autoComplete="new-password"
                required
                value={confirm}
                onChange={(event) => setConfirm(event.target.value)}
              />
            </Field>
          </div>
          {mismatch ? <p className="text-sm text-danger">{t("The passphrases differ.")}</p> : null}
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={includeHistory}
              onChange={(event) => setIncludeHistory(event.target.checked)}
            />
            {t("Include log lines, metric samples, and sessions (larger)")}
          </label>
          <Button type="submit" disabled={backup.isPending || mismatch}>
            {backup.isPending ? t("Creating backup…") : t("Download panel backup")}
          </Button>
        </form>
        <PanelRestoreHint />
      </Card>
      <Card title={t("Scheduled panel backups")}>
        <p className="mb-3 text-sm text-muted">
          {t(
            "Upload panel backups or settings exports to S3 on a schedule. Runs appear on the Backups page.",
          )}
        </p>
        <BackupJobForm target={{ panel: true }} />
      </Card>
      <Card title={t("Settings export")}>
        <p className="mb-3 text-sm text-muted">
          {t(
            "The panel configuration as JSON for review or version control. Passwords, tokens, keys, and environment values are left out, so it cannot be used to restore.",
          )}
        </p>
        <Button quiet onClick={() => exportSettings.mutate()} disabled={exportSettings.isPending}>
          {t("Download settings export")}
        </Button>
      </Card>
    </>
  );
}
