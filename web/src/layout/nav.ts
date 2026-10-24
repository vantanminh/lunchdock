import type { IconName } from "../components/icons";
import { msg } from "../i18n";

export type NavItem = readonly [to: string, label: string, icon: IconName];

export interface NavGroup {
  label: string;
  items: readonly NavItem[];
}

/** The sidebar, in order; the command palette lists the same pages. */
export const NAV_GROUPS: readonly NavGroup[] = [
  {
    label: msg("Deploy"),
    items: [
      ["/", msg("Overview"), "overview"],
      ["/projects", msg("Projects"), "folder"],
      ["/templates", msg("Templates"), "layers"],
      ["/servers", msg("Servers"), "server"],
    ],
  },
  {
    label: msg("Operate"),
    items: [
      ["/monitoring", msg("Monitoring"), "activity"],
      ["/load", msg("Load"), "gauge"],
      ["/alerts", msg("Alerts"), "bell"],
      ["/logs", msg("Logs"), "logs"],
      ["/backups", msg("Backups"), "archive"],
      ["/cache", msg("Cache"), "zap"],
    ],
  },
  {
    label: msg("Connections"),
    items: [
      ["/s3-destinations", msg("S3 destinations"), "bucket"],
      ["/certificates", msg("Certificates"), "shield"],
      ["/registries", msg("Registries"), "box"],
      ["/git", msg("Git providers"), "git"],
    ],
  },
  {
    label: msg("Admin"),
    items: [
      ["/users", msg("Users"), "users"],
      ["/tokens", msg("API tokens"), "key"],
      ["/agents", msg("AI agents"), "command"],
      ["/audit", msg("Audit log"), "history"],
      ["/system", msg("System"), "settings"],
    ],
  },
];
