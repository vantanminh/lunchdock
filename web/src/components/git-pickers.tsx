import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api } from "../api";
import { providerReady } from "../git";
import { useT } from "../i18n";
import { BRANCHES, COMPOSE_FILES } from "../presets";
import type { Schemas } from "../types";
import { Field, QuickPick, Select, TextInput, type PickOption } from "./ui";

type GitProvider = Schemas["GitProviderView"];
type Repo = Schemas["GithubRepoView"];

const OTHER = "__other__";

/** The GitHub App behind a provider can list what it reads; other providers are typed by hand. */
export function canBrowse(provider: GitProvider | undefined): provider is GitProvider {
  return provider?.kind === "github_app" && providerReady(provider);
}

/**
 * A select of values read from GitHub with an "other" choice that turns into a text box. Without
 * a list (another provider kind, GitHub unreachable) it is a text box with the usual presets.
 */
function PickOrType({
  label,
  options,
  value,
  onChange,
  placeholder,
  otherLabel,
  presets,
  presetsLabel,
  hint,
}: {
  label: string;
  /** Values the app can read; undefined while loading or unavailable. */
  options: PickOption[] | undefined;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  otherLabel: string;
  presets: readonly PickOption[];
  presetsLabel: string;
  hint?: string;
}) {
  const t = useT();
  const [manual, setManual] = useState(false);
  if (!options) {
    return (
      <Field
        label={label}
        hint={hint}
        presets={<QuickPick label={presetsLabel} options={presets} value={value} onPick={onChange} />}
      >
        <TextInput
          required
          value={value}
          placeholder={placeholder}
          onChange={(event) => onChange(event.target.value)}
        />
      </Field>
    );
  }
  const listed = options.some((option) => option.value === value);
  const typing = manual || (value !== "" && !listed);
  return (
    <div className="grid content-start gap-1.5">
      <Field label={label} hint={hint}>
        <Select
          required={!typing}
          value={typing ? OTHER : value}
          onChange={(event) => {
            if (event.target.value === OTHER) {
              setManual(true);
            } else {
              setManual(false);
              onChange(event.target.value);
            }
          }}
        >
          <option value="">{t("Choose")}</option>
          {options.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
          <option value={OTHER}>{otherLabel}</option>
        </Select>
      </Field>
      {typing ? (
        <TextInput
          required
          aria-label={label}
          value={value}
          placeholder={placeholder}
          onChange={(event) => onChange(event.target.value)}
        />
      ) : null}
    </div>
  );
}

/** Repositories the GitHub App can read. Picking one also offers its default branch. */
export function RepositoryField({
  provider,
  value,
  onChange,
  placeholder,
}: {
  provider: GitProvider | undefined;
  value: string;
  /** `defaultBranch` is set only when the repository was picked from the list. */
  onChange: (repo: string, defaultBranch?: string) => void;
  placeholder: string;
}) {
  const t = useT();
  const browse = canBrowse(provider);
  const repos = useQuery({
    queryKey: ["git-repositories", provider?.id],
    queryFn: () => api<Repo[]>(`/api/v1/git-providers/${provider?.id}/repositories`),
    enabled: browse,
    retry: false,
    staleTime: 60_000,
  });
  const options = repos.data?.map((repo) => ({
    label: repo.private ? `${repo.full_name} (${t("private")})` : repo.full_name,
    value: repo.url,
  }));
  return (
    <PickOrType
      label={t("Repository")}
      options={browse ? options : undefined}
      value={value}
      placeholder={placeholder}
      otherLabel={t("Another repository…")}
      presets={[]}
      presetsLabel={t("Repository")}
      hint={
        browse && repos.isError
          ? t("Could not list the repositories; type the address instead.")
          : undefined
      }
      onChange={(url) => {
        const repo = repos.data?.find((item) => item.url === url);
        onChange(url, repo?.default_branch || undefined);
      }}
    />
  );
}

/** Branches of the chosen repository, default first. */
export function BranchField({
  provider,
  repo,
  value,
  onChange,
}: {
  provider: GitProvider | undefined;
  repo: string;
  value: string;
  onChange: (branch: string) => void;
}) {
  const t = useT();
  const browse = canBrowse(provider) && repo.trim() !== "";
  const branches = useQuery({
    queryKey: ["git-branches", provider?.id, repo],
    queryFn: () =>
      api<Schemas["GithubBranchesView"]>(
        `/api/v1/git-providers/${provider?.id}/branches?repo=${encodeURIComponent(repo)}`,
      ),
    enabled: browse,
    retry: false,
    staleTime: 60_000,
  });
  const options = branches.data?.branches.map((name) => ({ label: name, value: name }));
  return (
    <PickOrType
      label={t("Branch")}
      options={browse ? options : undefined}
      value={value}
      otherLabel={t("Another branch…")}
      presets={BRANCHES}
      presetsLabel={t("Branch presets")}
      hint={browse && branches.isError ? t("The GitHub App cannot read this repository.") : undefined}
      onChange={onChange}
    />
  );
}

/** Compose files found in the repository. A fresh pick selects the first one. */
export function ComposeFileField({
  provider,
  repo,
  branch,
  value,
  onChange,
}: {
  provider: GitProvider | undefined;
  repo: string;
  branch: string;
  value: string;
  onChange: (path: string) => void;
}) {
  const t = useT();
  const browse = canBrowse(provider) && repo.trim() !== "" && branch.trim() !== "";
  const files = useQuery({
    queryKey: ["git-compose-files", provider?.id, repo, branch],
    queryFn: () =>
      api<Schemas["GithubComposeFilesView"]>(
        `/api/v1/git-providers/${provider?.id}/compose-files?repo=${encodeURIComponent(repo)}&branch=${encodeURIComponent(branch)}`,
      ),
    enabled: browse,
    retry: false,
    staleTime: 60_000,
  });
  const found = files.data?.files;
  useEffect(() => {
    const first = found?.[0];
    if (first && value === "") {
      onChange(first);
    }
  }, [found, value, onChange]);
  const options = found?.map((path) => ({ label: path, value: path }));
  return (
    <PickOrType
      label={t("Compose file path")}
      options={browse ? options : undefined}
      value={value}
      otherLabel={t("Another path…")}
      presets={COMPOSE_FILES}
      presetsLabel={t("Compose file presets")}
      hint={
        browse && found && found.length === 0
          ? t("No compose file was found on this branch; type its path.")
          : undefined
      }
      onChange={onChange}
    />
  );
}
