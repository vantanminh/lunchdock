import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { useAuth } from "../auth";
import { Icon } from "../components/icons";
import {
  Alert,
  Badge,
  Button,
  Card,
  Disclosure,
  Empty,
  Field,
  PageHeader,
  PickField,
  TextArea,
  TextInput,
} from "../components/ui";
import { errorMessage } from "../format";
import { msg, useT } from "../i18n";
import type { Schemas } from "../types";

type GitProvider = Schemas["GitProviderView"];
type Manifest = Schemas["GithubAppManifest"];
type InstallLink = Schemas["InstallLink"];

// Provider names are service-style names: lowercase letters, digits and dashes.
const KINDS = [
  { value: "github_pat", label: msg("GitHub token"), name: "github" },
  { value: "github_app", label: msg("GitHub App"), name: "github-app" },
  { value: "gitlab_pat", label: msg("GitLab token"), name: "gitlab" },
  { value: "generic_ssh", label: msg("SSH deploy key"), name: "deploy-key" },
];

const KIND_LABELS: Record<string, string> = Object.fromEntries(
  KINDS.map((kind) => [kind.value, kind.label]),
);

/** GitHub only accepts the manifest as a form POST from the browser. */
function sendToGitHub(manifest: Manifest) {
  const form = document.createElement("form");
  form.method = "POST";
  form.action = manifest.action_url;
  const field = document.createElement("input");
  field.type = "hidden";
  field.name = "manifest";
  field.value = manifest.manifest;
  form.append(field);
  document.body.append(form);
  form.submit();
}

export function GitProvidersPage() {
  const t = useT();
  const { user } = useAuth();
  const isAdmin = user?.role === "admin";
  const providers = useQuery({
    queryKey: ["git-providers"],
    queryFn: () => api<GitProvider[]>("/api/v1/git-providers"),
  });
  const list = providers.data ?? [];

  return (
    <>
      <PageHeader
        title={t("Git providers")}
        description={t("The accounts Rudder clones your code from. Services pick one as their source.")}
      />
      {isAdmin ? <ConnectGitHubCard /> : null}
      <Card
        title={t("Connected providers")}
        flush
        actions={
          providers.isSuccess ? (
            <span className="text-[13px] text-muted">{t("{count} total", { count: list.length })}</span>
          ) : null
        }
      >
        {list.length === 0 && !providers.isLoading ? (
          <Empty icon="git" title={t("No git providers yet")}>
            {t("Connect GitHub above, or add a token or deploy key by hand.")}
          </Empty>
        ) : (
          <ul className="divide-y divide-line border-t border-line">
            {list.map((provider) => (
              <ProviderRow key={provider.id} provider={provider} isAdmin={isAdmin} />
            ))}
          </ul>
        )}
      </Card>
      {isAdmin ? (
        <Disclosure title={t("Add a token, deploy key or existing GitHub App by hand")}>
          <ManualProviderForm />
        </Disclosure>
      ) : null}
    </>
  );
}

function ProviderRow({ provider, isAdmin }: { provider: GitProvider; isAdmin: boolean }) {
  const t = useT();
  const queryClient = useQueryClient();
  const pending = provider.status === "pending_install";
  const isApp = provider.kind === "github_app";

  const install = useMutation({
    mutationFn: () =>
      api<InstallLink>(`/api/v1/git-providers/${provider.id}/install-link`, { method: "POST" }),
    onSuccess: (link) => window.location.assign(link.install_url),
    onError: (error) => toast.error(errorMessage(error, t("Could not open GitHub"))),
  });
  const remove = useMutation({
    mutationFn: () => api(`/api/v1/git-providers/${provider.id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success(t("Git provider removed"));
      await queryClient.invalidateQueries({ queryKey: ["git-providers"] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not remove the provider"))),
  });

  return (
    <li className="flex flex-wrap items-center gap-x-4 gap-y-3 px-5 py-3.5 sm:px-6">
      <span className="inline-flex size-9 shrink-0 items-center justify-center rounded-alert border border-line bg-paper text-ink-soft">
        <Icon name="git" size={18} />
      </span>
      <div className="min-w-0 flex-1 basis-44">
        <p className="truncate font-medium text-ink">{provider.name}</p>
        <p className="truncate text-[13px] text-muted">
          {t(KIND_LABELS[provider.kind] ?? provider.kind)}
          {provider.account ? ` · ${provider.account}` : ""}
        </p>
      </div>
      {pending ? (
        <Badge value="Needs install" tone="warning" />
      ) : (
        <Badge value="Ready" tone="success" />
      )}
      {isAdmin ? (
        <span className="inline-flex flex-wrap items-center gap-2">
          {isApp && provider.app_slug ? (
            <Button
              size="sm"
              variant={pending ? "primary" : "secondary"}
              icon="external"
              disabled={install.isPending}
              onClick={() => install.mutate()}
            >
              {pending ? t("Finish installing") : t("Manage repositories")}
            </Button>
          ) : null}
          <Button
            size="sm"
            variant="danger"
            icon="trash"
            aria-label={t("Delete {name}", { name: provider.name })}
            disabled={remove.isPending}
            onClick={() => {
              const note = isApp ? t("The app stays on GitHub; delete it there as well.") : "";
              const question = t("Delete {name}? Services that use it can no longer pull code.", {
                name: provider.name,
              });
              if (window.confirm(`${question} ${note}`.trim())) {
                remove.mutate();
              }
            }}
          />
        </span>
      ) : null}
    </li>
  );
}

/** One click: GitHub creates the app from a manifest, the user installs it, and it lands here ready. */
function ConnectGitHubCard() {
  const t = useT();
  const [organization, setOrganization] = useState("");
  const [held, setHeld] = useState<Manifest | null>(null);

  const start = useMutation({
    mutationFn: () =>
      api<Manifest>("/api/v1/github-app/manifest", {
        method: "POST",
        body: JSON.stringify({ organization: organization.trim() || undefined }),
      }),
    onSuccess: (manifest) => {
      if (manifest.webhook) {
        sendToGitHub(manifest);
      } else {
        setHeld(manifest);
      }
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not start the GitHub setup"))),
  });

  const steps = [
    t("Confirm the new app on GitHub"),
    t("Choose the repositories Rudder may read"),
    t("Come back here, ready to deploy"),
  ];

  return (
    <Card
      title={t("Connect GitHub")}
      description={t(
        "Rudder creates a GitHub App for you. There is no key or id to copy, and it only gets read access to code.",
      )}
    >
      <div className="grid gap-4">
        <ol className="grid gap-2 text-sm text-ink-soft sm:grid-cols-3">
          {steps.map((step, index) => (
            <li
              key={step}
              className="flex items-start gap-2.5 rounded-alert border border-line bg-paper/60 px-3 py-2.5"
            >
              <span className="mt-px inline-flex size-5 shrink-0 items-center justify-center rounded-full bg-pine-soft text-xs font-semibold text-pine">
                {index + 1}
              </span>
              {step}
            </li>
          ))}
        </ol>
        {held ? (
          <Alert
            tone="warning"
            title={t("GitHub cannot reach this panel")}
            action={
              <Button size="sm" onClick={() => sendToGitHub(held)}>
                {t("Continue anyway")}
              </Button>
            }
          >
            {t(
              "Its address is private, so pushes will not start builds on their own. Set RUDDER_PUBLIC_URL to a public address to turn that on; you can still build by hand.",
            )}
          </Alert>
        ) : null}
        <Disclosure title={t("Create it under an organization")}>
          <Field
            label={t("GitHub organization")}
            hint={t("Leave empty to create the app under your own account.")}
          >
            <TextInput
              value={organization}
              placeholder="acme-inc"
              autoComplete="off"
              onChange={(event) => setOrganization(event.target.value)}
            />
          </Field>
        </Disclosure>
        <div>
          <Button icon="git" disabled={start.isPending} onClick={() => start.mutate()}>
            {start.isPending ? t("Opening GitHub…") : t("Connect with GitHub")}
          </Button>
        </div>
      </div>
    </Card>
  );
}

function ManualProviderForm() {
  const t = useT();
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [kind, setKind] = useState("github_pat");
  const [token, setToken] = useState("");
  const [appId, setAppId] = useState("");
  const [installationId, setInstallationId] = useState("");
  const [privateKey, setPrivateKey] = useState("");

  const create = useMutation({
    mutationFn: () =>
      api("/api/v1/git-providers", {
        method: "POST",
        body: JSON.stringify({
          name,
          kind,
          token: token || undefined,
          app_id: appId || undefined,
          installation_id: installationId || undefined,
          private_key_pem: privateKey || undefined,
        }),
      }),
    onSuccess: async () => {
      setToken("");
      setPrivateKey("");
      toast.success(t("Git provider saved"));
      await queryClient.invalidateQueries({ queryKey: ["git-providers"] });
    },
    onError: (error) => toast.error(errorMessage(error, t("Could not save"))),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <form className="grid gap-3" onSubmit={onSubmit}>
      <div className="grid gap-3 md:grid-cols-2">
        <Field label={t("Name")} hint={t("Lowercase letters, digits and dashes.")}>
          <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <PickField
          label={t("Kind")}
          hint={
            kind === "github_pat"
              ? t("A personal access token with read access to the repositories.")
              : kind === "generic_ssh"
                ? t(
                    "Each service gets its own key; its public key is shown after you save the service source.",
                  )
                : undefined
          }
          options={KINDS}
          value={kind}
          onPick={(next) => {
            const previous = KINDS.find((item) => item.value === kind);
            setKind(next);
            if (!name || name === previous?.name) {
              setName(KINDS.find((item) => item.value === next)?.name ?? "");
            }
          }}
        />
      </div>
      {kind === "github_pat" || kind === "gitlab_pat" ? (
        <Field label={t("Token")}>
          <TextInput type="password" value={token} onChange={(event) => setToken(event.target.value)} />
        </Field>
      ) : null}
      {kind === "github_app" ? (
        <>
          <div className="grid gap-3 md:grid-cols-2">
            <Field label={t("App id")}>
              <TextInput value={appId} onChange={(event) => setAppId(event.target.value)} />
            </Field>
            <Field label={t("Installation id")}>
              <TextInput
                value={installationId}
                onChange={(event) => setInstallationId(event.target.value)}
              />
            </Field>
          </div>
          <Field label={t("Private key PEM")}>
            <TextArea value={privateKey} onChange={(event) => setPrivateKey(event.target.value)} />
          </Field>
        </>
      ) : null}
      <div>
        <Button type="submit" disabled={create.isPending}>
          {t("Save")}
        </Button>
      </div>
    </form>
  );
}
