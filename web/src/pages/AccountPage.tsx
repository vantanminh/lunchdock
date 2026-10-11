import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError, setCsrf } from "../api";
import { useAuth } from "../auth";
import { Button, Card, Field, TextInput } from "../components/ui";

export function AccountPage() {
  const { user, refresh } = useAuth();
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
      toast.success("Password changed. Sign in again.");
      await refresh();
    } catch (error) {
      toast.error(error instanceof ApiError ? error.message : "Could not change password");
    }
  }

  async function startTotp() {
    try {
      const body = await api<{ secret: string; otpauth_url: string }>("/api/v1/me/totp/setup", {
        method: "POST",
      });
      setSetup(body);
    } catch (error) {
      toast.error(error instanceof ApiError ? error.message : "Could not start authenticator setup");
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
      toast.success("Authenticator enabled");
      await refresh();
    } catch (error) {
      toast.error(error instanceof ApiError ? error.message : "Code did not match");
    }
  }

  async function disableTotp(event: FormEvent) {
    event.preventDefault();
    try {
      await api("/api/v1/me/totp/disable", {
        method: "POST",
        body: JSON.stringify({ password: disablePassword, code: disableCode }),
      });
      toast.success("Authenticator disabled");
      await refresh();
    } catch (error) {
      toast.error(error instanceof ApiError ? error.message : "Could not disable authenticator");
    }
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">Account</h1>
      <Card title="Password">
        <form className="grid max-w-md gap-3" onSubmit={(event) => void changePassword(event)}>
          <Field label="Current password">
            <TextInput
              type="password"
              required
              value={currentPassword}
              onChange={(event) => setCurrentPassword(event.target.value)}
            />
          </Field>
          <Field label="New password">
            <TextInput
              type="password"
              required
              minLength={12}
              value={newPassword}
              onChange={(event) => setNewPassword(event.target.value)}
            />
          </Field>
          <Button type="submit">Change password</Button>
        </form>
      </Card>
      <Card title="Authenticator">
        {user?.totp_enabled ? (
          <form className="grid max-w-md gap-3" onSubmit={(event) => void disableTotp(event)}>
            <p className="text-sm text-muted">Authenticator codes are required at sign-in.</p>
            <Field label="Password">
              <TextInput
                type="password"
                required
                value={disablePassword}
                onChange={(event) => setDisablePassword(event.target.value)}
              />
            </Field>
            <Field label="Current code">
              <TextInput required value={disableCode} onChange={(event) => setDisableCode(event.target.value)} />
            </Field>
            <Button type="submit" quiet>
              Disable authenticator
            </Button>
          </form>
        ) : (
          <div className="space-y-4">
            <Button quiet onClick={() => void startTotp()}>
              Set up authenticator
            </Button>
            {setup ? (
              <form className="grid max-w-md gap-3" onSubmit={(event) => void enableTotp(event)}>
                <p className="break-all font-mono text-sm">{setup.secret}</p>
                <a className="text-sm text-accent underline" href={setup.otpauth_url}>
                  Open in authenticator app
                </a>
                <Field label="Code">
                  <TextInput required value={code} onChange={(event) => setCode(event.target.value)} />
                </Field>
                <Button type="submit">Enable</Button>
              </form>
            ) : null}
          </div>
        )}
      </Card>
    </>
  );
}
