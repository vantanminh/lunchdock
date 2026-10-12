import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError, setCsrf } from "../api";
import { useAuth } from "../auth";
import { Button, Card, Field, Segmented, TextInput } from "../components/ui";
import { LANGUAGES, translate, useLanguage, type Language } from "../i18n";
import type { Schemas } from "../types";

export function AccountPage() {
  const { user, refresh } = useAuth();
  const { language, setLanguage, t } = useLanguage();
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [setup, setSetup] = useState<{ secret: string; otpauth_url: string } | null>(null);
  const [code, setCode] = useState("");
  const [disablePassword, setDisablePassword] = useState("");
  const [disableCode, setDisableCode] = useState("");

  async function changePassword(event: FormEvent) {
    event.preventDefault();
    try {
      await api("/api/v1/me/password", {
        method: "POST",
        body: JSON.stringify({
          current_password: currentPassword,
          new_password: newPassword,
        }),
      });
      setCsrf(null);
      toast.success(t("Password changed. Sign in again."));
      await refresh();
    } catch (error) {
      toast.error(error instanceof ApiError ? error.message : t("Could not change password"));
    }
  }

  async function startTotp() {
    try {
      const body = await api<Schemas["TotpSetupResponse"]>("/api/v1/me/totp/setup", {
        method: "POST",
      });
      setSetup(body);
    } catch (error) {
      toast.error(
        error instanceof ApiError ? error.message : t("Could not start authenticator setup"),
      );
    }
  }

  async function enableTotp(event: FormEvent) {
    event.preventDefault();
    try {
      await api("/api/v1/me/totp/enable", {
        method: "POST",
        body: JSON.stringify({ code }),
      });
      setSetup(null);
      setCode("");
      toast.success(t("Authenticator enabled"));
      await refresh();
    } catch (error) {
      toast.error(error instanceof ApiError ? error.message : t("Code did not match"));
    }
  }

  async function disableTotp(event: FormEvent) {
    event.preventDefault();
    try {
      await api("/api/v1/me/totp/disable", {
        method: "POST",
        body: JSON.stringify({ password: disablePassword, code: disableCode }),
      });
      toast.success(t("Authenticator disabled"));
      await refresh();
    } catch (error) {
      toast.error(error instanceof ApiError ? error.message : t("Could not disable authenticator"));
    }
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">{t("Account")}</h1>
      <Card
        title={t("Language")}
        description={t("Choose the language used for menus, forms, and messages in this browser.")}
      >
        <Segmented
          label={t("Language")}
          value={language}
          options={LANGUAGES.map((item) => [item.code, item.label] as const)}
          onChange={(next: Language) => {
            setLanguage(next);
            // Translate with the new language, since this render still holds the old one.
            toast.success(String(translate(next, "Language changed")));
          }}
        />
      </Card>
      <Card title={t("Password")}>
        <form className="grid max-w-md gap-3" onSubmit={(event) => void changePassword(event)}>
          <Field label={t("Current password")}>
            <TextInput
              type="password"
              required
              value={currentPassword}
              onChange={(event) => setCurrentPassword(event.target.value)}
            />
          </Field>
          <Field label={t("New password")}>
            <TextInput
              type="password"
              required
              minLength={12}
              value={newPassword}
              onChange={(event) => setNewPassword(event.target.value)}
            />
          </Field>
          <Button type="submit">{t("Change password")}</Button>
        </form>
      </Card>
      <Card title={t("Authenticator")}>
        {user?.totp_enabled ? (
          <form className="grid max-w-md gap-3" onSubmit={(event) => void disableTotp(event)}>
            <p className="text-sm text-muted">
              {t("Authenticator codes are required at sign-in.")}
            </p>
            <Field label={t("Password")}>
              <TextInput
                type="password"
                required
                value={disablePassword}
                onChange={(event) => setDisablePassword(event.target.value)}
              />
            </Field>
            <Field label={t("Current code")}>
              <TextInput
                required
                value={disableCode}
                onChange={(event) => setDisableCode(event.target.value)}
              />
            </Field>
            <Button type="submit" quiet>
              {t("Disable authenticator")}
            </Button>
          </form>
        ) : (
          <div className="space-y-4">
            <Button quiet onClick={() => void startTotp()}>
              {t("Set up authenticator")}
            </Button>
            {setup ? (
              <form className="grid max-w-md gap-3" onSubmit={(event) => void enableTotp(event)}>
                <p className="break-all font-mono text-sm">{setup.secret}</p>
                <a className="text-sm text-accent underline" href={setup.otpauth_url}>
                  {t("Open in authenticator app")}
                </a>
                <Field label={t("Code")}>
                  <TextInput
                    required
                    value={code}
                    onChange={(event) => setCode(event.target.value)}
                  />
                </Field>
                <Button type="submit">{t("Enable")}</Button>
              </form>
            ) : null}
          </div>
        )}
      </Card>
    </>
  );
}
