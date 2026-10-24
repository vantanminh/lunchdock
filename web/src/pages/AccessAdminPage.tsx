import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Button, Card, Field, TextInput } from "../components/ui";
import { useT } from "../i18n";
import type { Schemas } from "../types";

type Settings = Schemas["AccessSettingsView"];
type Department = Schemas["AccessDepartment"];
type Rank = Schemas["AccessRank"];
type Person = Schemas["AccessIdentity"];

export function AccessAdminPage() {
  const t = useT();
  const queryClient = useQueryClient();
  const settings = useQuery({
    queryKey: ["access-settings"],
    queryFn: () => api<Settings>("/api/v1/access/settings"),
  });
  const departments = useQuery({
    queryKey: ["access-departments"],
    queryFn: () => api<Department[]>("/api/v1/access/departments"),
  });
  const ranks = useQuery({
    queryKey: ["access-ranks"],
    queryFn: () => api<Rank[]>("/api/v1/access/ranks"),
  });
  const people = useQuery({
    queryKey: ["access-people"],
    queryFn: () => api<Person[]>("/api/v1/access/identities"),
  });

  return (
    <>
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-2xl font-semibold">{t("Rudder Access")}</h1>
          <p className="text-sm text-muted">
            {t("Decide who may open internal apps. People sign in at the Access portal, separate from this panel.")}
          </p>
        </div>
        <Link className="text-sm text-pine" to="/access">
          {t("Open the Access portal")}
        </Link>
      </div>
      {settings.data ? <SettingsForm current={settings.data} /> : null}
      <Directory
        title={t("Departments")}
        description={t("A grant for a department also covers the departments under it.")}
        rows={(departments.data ?? []).map((item) => item.name)}
        onCreate={async (name, slug) => {
          await api("/api/v1/access/departments", {
            method: "POST",
            body: JSON.stringify({ name, slug }),
          });
          await queryClient.invalidateQueries({ queryKey: ["access-departments"] });
        }}
      />
      <Directory
        title={t("Ranks")}
        description={t("Internal apps can map these ranks onto their own roles.")}
        rows={(ranks.data ?? []).map((item) => `${item.name} (${item.level})`)}
        extra
        onCreate={async (name, slug, level) => {
          await api("/api/v1/access/ranks", {
            method: "POST",
            body: JSON.stringify({ name, slug, level: Number(level) }),
          });
          await queryClient.invalidateQueries({ queryKey: ["access-ranks"] });
        }}
      />
      <People
        people={people.data ?? []}
        departments={departments.data ?? []}
        ranks={ranks.data ?? []}
      />
    </>
  );
}

function SettingsForm({ current }: { current: Settings }) {
  const t = useT();
  const queryClient = useQueryClient();
  const [siteKey, setSiteKey] = useState(current.turnstile_site_key);
  const [secret, setSecret] = useState("");
  const [hosts, setHosts] = useState(current.turnstile_hostnames.join(", "));
  const [portal, setPortal] = useState(current.portal_host ?? "");
  const [provider, setProvider] = useState(current.email_provider);
  const [from, setFrom] = useState(current.email_from);
  const [resend, setResend] = useState("");
  const [smtpHost, setSmtpHost] = useState("");
  const [account, setAccount] = useState("");
  const [token, setToken] = useState("");
  const [googleId, setGoogleId] = useState(current.google_client_id);
  const [googleSecret, setGoogleSecret] = useState("");

  const save = useMutation({
    mutationFn: () =>
      api("/api/v1/access/settings", {
        method: "PUT",
        body: JSON.stringify({
          portal_host: portal,
          clear_portal_host: portal.trim() === "",
          turnstile_site_key: siteKey,
          turnstile_secret: secret || undefined,
          turnstile_hostnames: hosts.split(",").map((item) => item.trim()).filter(Boolean),
          google_client_id: googleId,
          google_client_secret: googleSecret || undefined,
          email_provider: provider,
          email_from: from,
          resend_api_key: resend || undefined,
          smtp_host: smtpHost || undefined,
          cloudflare_account_id: account || undefined,
          cloudflare_api_token: token || undefined,
        }),
      }),
    onSuccess: async () => {
      toast.success(t("Access settings saved"));
      setSecret("");
      setResend("");
      setGoogleSecret("");
      setToken("");
      await queryClient.invalidateQueries({ queryKey: ["access-settings"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not save Access settings")),
  });

  const test = useMutation({
    mutationFn: () => api("/api/v1/access/settings/test-email", { method: "POST" }),
    onSuccess: () => toast.success(t("Test email sent")),
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not send the test email")),
  });

  return (
    <Card
      title={t("Sign-in")}
      description={t("Turnstile is required. Google and email codes use one redirect address.")}
    >
      <form
        className="grid gap-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field label={t("Portal hostname")} hint={t("Leave blank to use this panel at /access.")}>
          <TextInput value={portal} placeholder="access.example.com" onChange={(event) => setPortal(event.target.value)} />
        </Field>
        <Field label={t("Google redirect URI")}>
          <TextInput readOnly value={current.google_redirect_uri} />
        </Field>
        <Field label={t("Turnstile site key")}>
          <TextInput value={siteKey} onChange={(event) => setSiteKey(event.target.value)} />
        </Field>
        <Field
          label={t("Turnstile secret")}
          hint={current.has_turnstile_secret ? t("A secret is saved. Leave blank to keep it.") : t("Required before anyone can sign in.")}
        >
          <TextInput type="password" value={secret} onChange={(event) => setSecret(event.target.value)} />
        </Field>
        <Field label={t("Turnstile hostnames")} hint={t("Comma-separated. Do not include localhost in production.")}>
          <TextInput value={hosts} onChange={(event) => setHosts(event.target.value)} />
        </Field>
        <Field label={t("Google client id")}>
          <TextInput value={googleId} onChange={(event) => setGoogleId(event.target.value)} />
        </Field>
        <Field label={t("Google client secret")}>
          <TextInput type="password" value={googleSecret} onChange={(event) => setGoogleSecret(event.target.value)} />
        </Field>
        <Field label={t("Email provider")}>
          <select
            className="h-10 rounded-md border border-line bg-paper px-2 text-sm"
            value={provider}
            onChange={(event) => setProvider(event.target.value)}
          >
            <option value="none">{t("None")}</option>
            <option value="resend">Resend</option>
            <option value="smtp">SMTP</option>
            <option value="cloudflare">{t("Cloudflare Email Sending")}</option>
          </select>
        </Field>
        <Field label={t("From address")}>
          <TextInput value={from} placeholder="Rudder Access <login@example.com>" onChange={(event) => setFrom(event.target.value)} />
        </Field>
        {provider === "resend" ? (
          <Field label={t("Resend API key")}>
            <TextInput type="password" value={resend} onChange={(event) => setResend(event.target.value)} />
          </Field>
        ) : null}
        {provider === "smtp" ? (
          <Field label={t("SMTP host")}>
            <TextInput value={smtpHost} onChange={(event) => setSmtpHost(event.target.value)} />
          </Field>
        ) : null}
        {provider === "cloudflare" ? (
          <>
            <Field label={t("Cloudflare account id")}>
              <TextInput value={account} onChange={(event) => setAccount(event.target.value)} />
            </Field>
            <Field label={t("Cloudflare API token")}>
              <TextInput type="password" value={token} onChange={(event) => setToken(event.target.value)} />
            </Field>
          </>
        ) : null}
        <div className="flex gap-2">
          <Button type="submit" disabled={save.isPending}>
            {t("Save")}
          </Button>
          <Button type="button" variant="secondary" disabled={test.isPending} onClick={() => test.mutate()}>
            {t("Send a test email")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

function Directory({
  title,
  description,
  rows,
  extra,
  onCreate,
}: {
  title: string;
  description: string;
  rows: string[];
  extra?: boolean;
  onCreate: (name: string, slug: string, level?: string) => Promise<void>;
}) {
  const t = useT();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [level, setLevel] = useState("10");
  return (
    <Card title={title} description={description}>
      <ul className="mb-4 space-y-1 text-sm">
        {rows.length === 0 ? <li className="text-muted">{t("None yet")}</li> : rows.map((row) => <li key={row}>{row}</li>)}
      </ul>
      <form
        className="grid gap-3 md:grid-cols-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault();
          void onCreate(name, slug, extra ? level : undefined)
            .then(() => {
              setName("");
              setSlug("");
              toast.success(t("Saved"));
            })
            .catch((error: unknown) =>
              toast.error(error instanceof ApiError ? error.message : t("Could not save")),
            );
        }}
      >
        <Field label={t("Name")}>
          <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <Field label={t("Slug")}>
          <TextInput required value={slug} onChange={(event) => setSlug(event.target.value)} />
        </Field>
        {extra ? (
          <Field label={t("Level")}>
            <TextInput value={level} onChange={(event) => setLevel(event.target.value)} />
          </Field>
        ) : null}
        <div className="flex items-end">
          <Button type="submit">{t("Add")}</Button>
        </div>
      </form>
    </Card>
  );
}

function People({
  people,
  departments,
  ranks,
}: {
  people: Person[];
  departments: Department[];
  ranks: Rank[];
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const [email, setEmail] = useState("");
  const [name, setName] = useState("");
  const [admin, setAdmin] = useState(false);
  const [memberId, setMemberId] = useState("");
  const [departmentId, setDepartmentId] = useState("");
  const [rankId, setRankId] = useState("");
  const create = useMutation({
    mutationFn: () =>
      api("/api/v1/access/identities", {
        method: "POST",
        body: JSON.stringify({ email, display_name: name, admin }),
      }),
    onSuccess: async () => {
      setEmail("");
      setName("");
      toast.success(t("Person added"));
      await queryClient.invalidateQueries({ queryKey: ["access-people"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : t("Could not add the person")),
  });
  return (
    <Card title={t("People")} description={t("Only these email addresses can sign in. Unknown addresses are blocked.")}>
      <form
        className="mb-4 grid gap-3 md:grid-cols-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault();
          create.mutate();
        }}
      >
        <Field label={t("Email")}>
          <TextInput type="email" required value={email} onChange={(event) => setEmail(event.target.value)} />
        </Field>
        <Field label={t("Name")}>
          <TextInput value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <label className="flex items-end gap-2 text-sm">
          <input type="checkbox" checked={admin} onChange={(event) => setAdmin(event.target.checked)} />
          {t("Access administrator")}
        </label>
        <div className="flex items-end">
          <Button type="submit">{t("Add")}</Button>
        </div>
      </form>
      <form
        className="mb-4 grid gap-3 md:grid-cols-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault();
          void api(`/api/v1/access/identities/${memberId}/memberships`, {
            method: "PUT",
            body: JSON.stringify({
              memberships: [
                { department_id: departmentId, rank_id: rankId || null },
              ],
            }),
          })
            .then(() => toast.success(t("Membership saved")))
            .catch((error: unknown) =>
              toast.error(error instanceof ApiError ? error.message : t("Could not save the membership")),
            );
        }}
      >
        <Field label={t("Person")}>
          <select
            className="h-10 rounded-md border border-line bg-paper px-2 text-sm"
            value={memberId}
            onChange={(event) => setMemberId(event.target.value)}
            required
          >
            <option value="">{t("Choose")}</option>
            {people.map((person) => (
              <option key={person.id} value={person.id}>
                {person.email}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t("Department")}>
          <select
            className="h-10 rounded-md border border-line bg-paper px-2 text-sm"
            value={departmentId}
            onChange={(event) => setDepartmentId(event.target.value)}
            required
          >
            <option value="">{t("Choose")}</option>
            {departments.map((department) => (
              <option key={department.id} value={department.id}>
                {department.name}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t("Rank")}>
          <select
            className="h-10 rounded-md border border-line bg-paper px-2 text-sm"
            value={rankId}
            onChange={(event) => setRankId(event.target.value)}
          >
            <option value="">{t("None")}</option>
            {ranks.map((rank) => (
              <option key={rank.id} value={rank.id}>
                {rank.name}
              </option>
            ))}
          </select>
        </Field>
        <div className="flex items-end">
          <Button type="submit">{t("Assign")}</Button>
        </div>
      </form>
      <ul className="divide-y divide-line">
        {people.map((person) => (
          <li key={person.id} className="flex items-center justify-between py-2 text-sm">
            <span>
              <span className="font-medium">{person.display_name}</span>
              <span className="ml-2 text-muted">{person.email}</span>
            </span>
            <span className="text-muted">
              {person.owner ? t("Owner") : person.admin ? t("Administrator") : person.status}
            </span>
          </li>
        ))}
      </ul>
    </Card>
  );
}
