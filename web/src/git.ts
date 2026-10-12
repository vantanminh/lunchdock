import type { Translate } from "./i18n";
import type { Schemas } from "./types";

type GitProvider = Schemas["GitProviderView"];

/** A GitHub App that is created but not installed yet cannot be used as a source. */
export const providerReady = (provider: GitProvider) => provider.status === "ready";

/** The text of a provider in a select, with a note when it cannot be used yet. */
export function providerLabel(provider: GitProvider, t: Translate): string {
  const label = `${provider.name} (${provider.kind})`;
  return providerReady(provider) ? label : `${label} · ${t("needs install")}`;
}
