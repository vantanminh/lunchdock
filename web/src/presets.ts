// One-click suggestions for settings that are tedious to type. Labels are marked with msg() so
// they are translated where they are rendered; values and plain labels are never translated.
import type { PickOption } from "./components/ui";
import { msg } from "./i18n";

export const plain = (values: readonly (string | number)[]): PickOption[] =>
  values.map((value) => ({ label: String(value), value: String(value) }));

/** Presets understood by the scheduler, plus a few common cron expressions. */
export const SCHEDULES: PickOption[] = [
  { label: msg("Hourly"), value: "@hourly", hint: msg("At minute 0 of every hour") },
  { label: msg("Every 6 hours"), value: "@every6h", hint: msg("00:00, 06:00, 12:00 and 18:00") },
  { label: msg("Daily 03:00"), value: "@daily", hint: msg("Every day at 03:00") },
  { label: msg("Daily 00:00"), value: "0 0 * * *", hint: msg("Every day at midnight") },
  { label: msg("Weekdays 02:00"), value: "0 2 * * 1-5", hint: msg("Monday to Friday at 02:00") },
  { label: msg("Weekly (Sun 03:00)"), value: "@weekly", hint: msg("Every Sunday at 03:00") },
  {
    label: msg("Monthly (1st, 03:00)"),
    value: "@monthly",
    hint: msg("On the 1st of each month at 03:00"),
  },
  { label: msg("Manual only"), value: "", hint: msg("Never runs on its own") },
];

export function timezoneOptions(detected: string): PickOption[] {
  const zones = [
    detected,
    "UTC",
    "Asia/Ho_Chi_Minh",
    "Asia/Singapore",
    "Asia/Tokyo",
    "Europe/London",
    "America/New_York",
  ];
  return plain([...new Set(zones.filter(Boolean))]);
}

/** Every IANA zone the browser knows, for the timezone field's autocomplete. */
export function allTimezones(): string[] {
  try {
    return (
      (Intl as unknown as { supportedValuesOf?: (key: string) => string[] }).supportedValuesOf?.(
        "timeZone",
      ) ?? []
    );
  } catch {
    return [];
  }
}

/** Seconds, for timeouts and "for at least" windows. */
export const DURATIONS: PickOption[] = [
  { label: msg("1 min"), value: "60" },
  { label: msg("5 min"), value: "300" },
  { label: msg("15 min"), value: "900" },
  { label: msg("30 min"), value: "1800" },
  { label: msg("1 hour"), value: "3600" },
  { label: msg("6 hours"), value: "21600" },
  { label: msg("24 hours"), value: "86400" },
];

export const pickDurations = (...values: string[]) =>
  DURATIONS.filter((option) => values.includes(option.value));

export const RETRIES = plain([0, 1, 2, 3, 5]);

export const BANDWIDTH: PickOption[] = [
  { label: msg("Unlimited"), value: "" },
  ...["10", "50", "100", "500"].map((value) => ({ label: `${value} Mbit/s`, value })),
];

export interface RetentionPreset {
  label: string;
  hint: string;
  values: { keep_last: string; keep_days: string; daily: string; weekly: string; monthly: string };
}

const none = { keep_last: "", keep_days: "", daily: "", weekly: "", monthly: "" };

export const RETENTION: RetentionPreset[] = [
  {
    label: msg("Last 7"),
    hint: msg("Keep the 7 newest backups"),
    values: { ...none, keep_last: "7" },
  },
  {
    label: msg("Last 14"),
    hint: msg("Keep the 14 newest backups"),
    values: { ...none, keep_last: "14" },
  },
  {
    label: msg("30 days"),
    hint: msg("Keep everything from the last 30 days"),
    values: { ...none, keep_days: "30" },
  },
  {
    label: msg("7 daily + 4 weekly + 6 monthly"),
    hint: msg("One backup per day for a week, per week for a month, per month for half a year"),
    values: { ...none, daily: "7", weekly: "4", monthly: "6" },
  },
  { label: msg("Keep everything"), hint: msg("Never delete old backups"), values: none },
];

export const retentionKey = (values: RetentionPreset["values"]) =>
  `${values.keep_last}|${values.keep_days}|${values.daily}|${values.weekly}|${values.monthly}`;

/** Compression levels per codec; zstd accepts 1-19 and gzip 1-9. */
export function compressionLevels(codec: string): PickOption[] {
  if (codec === "zstd") {
    return [
      { label: msg("Fast"), value: "1", hint: msg("Least CPU, largest files") },
      { label: msg("Balanced"), value: "3", hint: msg("Good default") },
      { label: msg("Small"), value: "9", hint: msg("More CPU, smaller files") },
      { label: msg("Smallest"), value: "19", hint: msg("Slow, smallest files") },
    ];
  }
  if (codec === "gzip") {
    return [
      { label: msg("Fast"), value: "1", hint: msg("Least CPU, largest files") },
      { label: msg("Balanced"), value: "6", hint: msg("Good default") },
      { label: msg("Smallest"), value: "9", hint: msg("Slow, smallest files") },
    ];
  }
  return [];
}

export const EXCLUDE_GLOBS = plain(["*.log", "*.tmp", "node_modules/**", ".cache/**", "tmp/**"]);

export const PORTS = plain([80, 3000, 3001, 5000, 8000, 8080]);

export const BRANCHES = plain(["main", "master", "develop", "production"]);

export const IMAGES = plain([
  "nginx:alpine",
  "caddy:2-alpine",
  "httpd:alpine",
  "node:22-alpine",
  "python:3.13-slim",
  "traefik/whoami",
]);

export const ENVIRONMENT_NAMES = plain(["prod", "staging", "dev", "preview"]);

export const COMPOSE_FILES = plain(["docker-compose.yml", "compose.yaml", "compose.prod.yml"]);

export const MAIN_SERVICES = plain(["web", "app", "api"]);

export const SERVER_NAMES = plain(["main", "prod", "staging", "edge-1"]);

export const TOKEN_NAMES = plain(["ci", "deploy-bot", "monitoring", "cli"]);

/** Common environment variables, with the values people usually give them. */
export const ENV_KEYS: { key: string; values: string[] }[] = [
  { key: "NODE_ENV", values: ["production", "development"] },
  { key: "PORT", values: ["3000", "8080"] },
  { key: "TZ", values: ["UTC", "Asia/Ho_Chi_Minh"] },
  { key: "LOG_LEVEL", values: ["info", "debug", "warn", "error"] },
  { key: "DATABASE_URL", values: [] },
  { key: "REDIS_URL", values: [] },
];

export interface S3Provider {
  label: string;
  endpoint: string;
  region: string;
  path_style: boolean;
}

/** Endpoint, region, and addressing style for the common S3-compatible services. */
export const S3_PROVIDERS: S3Provider[] = [
  { label: "AWS S3", endpoint: "", region: "us-east-1", path_style: false },
  {
    label: "Cloudflare R2",
    endpoint: "https://<account-id>.r2.cloudflarestorage.com",
    region: "auto",
    path_style: true,
  },
  {
    label: "Backblaze B2",
    endpoint: "https://s3.us-west-004.backblazeb2.com",
    region: "us-west-004",
    path_style: true,
  },
  { label: "Wasabi", endpoint: "https://s3.wasabisys.com", region: "us-east-1", path_style: true },
  {
    label: "DigitalOcean Spaces",
    endpoint: "https://nyc3.digitaloceanspaces.com",
    region: "nyc3",
    path_style: true,
  },
  { label: "MinIO", endpoint: "http://minio:9000", region: "us-east-1", path_style: true },
  { label: "RustFS", endpoint: "http://rustfs:9000", region: "us-east-1", path_style: true },
  { label: "Garage", endpoint: "http://garage:3900", region: "garage", path_style: true },
];

export const S3_REGIONS = plain(["us-east-1", "eu-central-1", "ap-southeast-1", "auto"]);

export const REGISTRIES: { label: string; url: string }[] = [
  { label: "Docker Hub", url: "docker.io" },
  { label: "GitHub", url: "ghcr.io" },
  { label: "GitLab", url: "registry.gitlab.com" },
  { label: "Quay", url: "quay.io" },
  { label: "DigitalOcean", url: "registry.digitalocean.com" },
];

export const SMTP_PORTS = plain([587, 465, 25]);

export interface SmtpProvider {
  label: string;
  host: string;
  port: string;
  security: "starttls" | "tls" | "none";
  username?: string;
}

export const SMTP_PROVIDERS: SmtpProvider[] = [
  { label: "Gmail", host: "smtp.gmail.com", port: "587", security: "starttls" },
  { label: "Outlook", host: "smtp.office365.com", port: "587", security: "starttls" },
  {
    label: "Amazon SES",
    host: "email-smtp.us-east-1.amazonaws.com",
    port: "587",
    security: "starttls",
  },
  {
    label: "SendGrid",
    host: "smtp.sendgrid.net",
    port: "587",
    security: "starttls",
    username: "apikey",
  },
  { label: "Mailgun", host: "smtp.mailgun.org", port: "587", security: "starttls" },
  { label: "Brevo", host: "smtp-relay.brevo.com", port: "587", security: "starttls" },
];

/** Alert rule names, thresholds, and event sets keyed by the rule's condition. */
export const ALERT_NAMES: Record<string, string> = {
  server_cpu: msg("High CPU"),
  server_memory: msg("High memory"),
  server_disk: msg("Disk almost full"),
  server_offline: msg("Server offline"),
  service_cpu: msg("Service high CPU"),
  service_memory: msg("Service high memory"),
  service_down: msg("Service down"),
  http_errors: msg("HTTP 5xx errors"),
  event: msg("Failure events"),
};

export function alertThresholds(kind: string): PickOption[] {
  if (kind === "service_memory") return plain([256, 512, 1024, 2048]);
  if (kind === "http_errors") return plain([5, 10, 50, 100]);
  return [70, 80, 90, 95].map((value) => ({ label: `${value}%`, value: String(value) }));
}

export const EVENT_SETS: PickOption[] = [
  {
    label: msg("All failures"),
    value: "deploy.failed, backup.failed, build.failed, update.failed",
  },
  { label: msg("Backups"), value: "backup.*" },
  { label: msg("Deployments"), value: "deploy.*, build.failed" },
  { label: msg("Servers"), value: "server.offline, server.online" },
  { label: msg("Everything"), value: "*" },
];

export const MAINTENANCE_WINDOWS: PickOption[] = [
  { label: msg("Any time"), value: "" },
  ...plain(["02:00-05:00", "03:00-05:00", "00:00-04:00"]),
];

export const CACHE_MEMORY_MIB = plain([64, 128, 256, 512, 1024]);

export const CACHE_OBJECT_MIB = plain([1, 4, 16, 64]);

export const PURGE_PATHS = plain(["/*", "/index.html", "/assets/*", "/static/*"]);

export const COMPOSE_STARTERS: { label: string; yaml: string }[] = [
  {
    label: msg("Web (nginx)"),
    yaml: `services:
  web:
    image: nginx:1.27-alpine
    restart: unless-stopped
`,
  },
  {
    label: msg("App + Postgres"),
    yaml: `services:
  web:
    image: ghcr.io/org/app:latest
    restart: unless-stopped
    environment:
      DATABASE_URL: postgres://app:\${DB_PASSWORD}@db:5432/app
    depends_on:
      - db
  db:
    image: postgres:16-alpine
    restart: unless-stopped
    environment:
      POSTGRES_USER: app
      POSTGRES_PASSWORD: \${DB_PASSWORD}
      POSTGRES_DB: app
    volumes:
      - db-data:/var/lib/postgresql/data
volumes:
  db-data:
`,
  },
  {
    label: msg("App + Redis"),
    yaml: `services:
  web:
    image: ghcr.io/org/app:latest
    restart: unless-stopped
    environment:
      REDIS_URL: redis://cache:6379
    depends_on:
      - cache
  cache:
    image: redis:7-alpine
    restart: unless-stopped
`,
  },
];

/** Fixed choices, shown as chips so every option is visible at once. */
export const SSL_MODES: PickOption[] = [
  {
    label: "Let's Encrypt",
    value: "letsencrypt",
    hint: msg("Free certificate, issued and renewed automatically"),
  },
  {
    label: msg("Uploaded certificate"),
    value: "custom",
    hint: msg("Use a certificate from the Certificates page"),
  },
  { label: msg("None"), value: "none", hint: msg("Plain HTTP only") },
];

export const BUILD_STRATEGIES: PickOption[] = [
  { label: "Dockerfile", value: "dockerfile", hint: msg("Build with the repository's Dockerfile") },
  {
    label: "Nixpacks",
    value: "nixpacks",
    hint: msg("Detect the language and build without a Dockerfile"),
  },
  { label: msg("Static site"), value: "static", hint: msg("Serve a folder of built files") },
];

export const ROLES: PickOption[] = [
  { label: msg("viewer"), value: "viewer", hint: msg("Can look but not change anything") },
  { label: msg("admin"), value: "admin", hint: msg("Can manage everything") },
];

export const SERVER_KINDS: PickOption[] = [
  { label: msg("local"), value: "local", hint: msg("The machine running the panel") },
  {
    label: msg("remote"),
    value: "remote",
    hint: msg("Another machine that runs the Rudder agent"),
  },
];

export const TOKEN_SCOPES: PickOption[] = [
  { label: msg("Read only"), value: "read", hint: msg("Can list and view, never change") },
  {
    label: msg("Read and write"),
    value: "write",
    hint: msg("Can also deploy and change settings"),
  },
];

export const SERVICE_KINDS: PickOption[] = [
  {
    label: msg("application"),
    value: "application",
    hint: msg("A container image or a Git repository"),
  },
  { label: msg("database"), value: "database", hint: msg("PostgreSQL, MySQL, Redis and more") },
  {
    label: msg("compose stack"),
    value: "compose",
    hint: msg("Several containers from one docker-compose file"),
  },
];

export const STACK_SOURCES: PickOption[] = [
  { label: msg("Paste YAML"), value: "inline" },
  { label: msg("Git repository (local server)"), value: "git" },
];

export const UPDATE_CHANNELS: PickOption[] = [
  { label: msg("Stable releases"), value: "stable" },
  { label: msg("Include pre-releases"), value: "prerelease" },
];

export const SEVERITIES: PickOption[] = [
  { label: msg("info"), value: "info" },
  { label: msg("warning"), value: "warning" },
  { label: msg("critical"), value: "critical" },
];

export interface MetricRetentionPreset {
  label: string;
  hint: string;
  values: { raw_hours: string; m1_days: string; m5_days: string; h1_days: string };
}

/** How long metric samples and their rollups are kept; "Standard" is the panel default. */
export const METRIC_RETENTION: MetricRetentionPreset[] = [
  {
    label: msg("Light"),
    hint: msg("Smallest database: 6 hours of detail, 3 days of 1-minute rollups"),
    values: { raw_hours: "6", m1_days: "3", m5_days: "7", h1_days: "14" },
  },
  {
    label: msg("Standard"),
    hint: msg("The default: 1 day of detail, 7 days of 1-minute rollups"),
    values: { raw_hours: "24", m1_days: "7", m5_days: "14", h1_days: "30" },
  },
  {
    label: msg("Long"),
    hint: msg("3 days of detail and a year of hourly history"),
    values: { raw_hours: "72", m1_days: "30", m5_days: "90", h1_days: "365" },
  },
];

export const LOG_DAYS: PickOption[] = [
  { label: msg("7 days"), value: "7" },
  { label: msg("14 days"), value: "14" },
  { label: msg("30 days"), value: "30" },
  { label: msg("90 days"), value: "90" },
];

export const YES_NO: PickOption[] = [
  { label: msg("Yes"), value: "true" },
  { label: msg("No"), value: "false" },
];

export const MEMORY_LIMITS_MIB: PickOption[] = [
  { label: msg("No limit"), value: "" },
  ...plain([256, 512, 1024, 2048]),
];

export const CPU_LIMITS: PickOption[] = [
  { label: msg("No limit"), value: "" },
  ...plain([0.5, 1, 2, 4]),
];
