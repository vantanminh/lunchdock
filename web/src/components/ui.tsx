import { Button as BaseButton } from "@base-ui/react/button";
import { Dialog as BaseDialog } from "@base-ui/react/dialog";
import { Input as BaseInput } from "@base-ui/react/input";
import { useState } from "react";
import type {
  ButtonHTMLAttributes,
  InputHTMLAttributes,
  ReactNode,
  SelectHTMLAttributes,
  TdHTMLAttributes,
  TextareaHTMLAttributes,
} from "react";
import { toast } from "sonner";
import { useT } from "../i18n";
import { Icon, type IconName } from "./icons";

type ButtonVariant = "primary" | "secondary" | "ghost" | "danger" | "danger-solid";
type ButtonSize = "sm" | "md" | "lg";

const buttonBase =
  "relative inline-flex shrink-0 items-center justify-center gap-2 rounded-control border font-medium whitespace-nowrap transition-colors duration-150 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-pine disabled:pointer-events-none disabled:opacity-55";

const buttonSizes: Record<ButtonSize, string> = {
  sm: "h-8 gap-1.5 px-2.5 text-[13px]",
  md: "h-10 px-3.5 text-sm",
  lg: "h-11 px-4 text-[15px]",
};

const buttonVariants: Record<ButtonVariant, string> = {
  primary:
    "border-transparent bg-pine text-pine-ink shadow-[inset_0_1px_0_rgb(255_255_255/0.08),0_1px_2px_rgb(28_25_23/0.18)] hover:bg-pine-hover",
  secondary:
    "border-line-strong bg-surface text-ink shadow-[0_1px_1px_rgb(28_25_23/0.04)] hover:border-faint/60 hover:bg-paper",
  ghost: "border-transparent bg-transparent text-ink-soft hover:bg-sunken hover:text-ink",
  danger:
    "border-danger-line bg-surface text-danger shadow-[0_1px_1px_rgb(28_25_23/0.04)] hover:bg-danger-soft",
  "danger-solid":
    "border-transparent bg-danger text-surface shadow-[inset_0_1px_0_rgb(255_255_255/0.08),0_1px_2px_rgb(28_25_23/0.18)] hover:bg-danger-hover",
};

export function buttonClass({
  variant = "primary",
  size = "md",
}: { variant?: ButtonVariant; size?: ButtonSize } = {}) {
  return `${buttonBase} ${buttonSizes[size]} ${buttonVariants[variant]}`;
}

export function Button({
  quiet = false,
  variant,
  size = "md",
  icon,
  className = "",
  children,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  /** Shorthand for the secondary variant. */
  quiet?: boolean;
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: IconName;
}) {
  return (
    <BaseButton
      className={`${buttonClass({ variant: variant ?? (quiet ? "secondary" : "primary"), size })} ${className}`}
      {...props}
    >
      {icon ? <Icon name={icon} size={size === "sm" ? 14 : 16} /> : null}
      {children}
    </BaseButton>
  );
}

const controlClass =
  "w-full rounded-control border border-line-strong bg-surface text-[15px] text-ink shadow-[inset_0_1px_1px_rgb(28_25_23/0.03)] outline-none transition-[border-color,box-shadow] duration-150 placeholder:text-faint hover:border-faint/70 focus:border-pine focus:shadow-[0_0_0_3px_color-mix(in_srgb,var(--pine)_14%,transparent)] disabled:bg-sunken disabled:text-muted aria-[invalid=true]:border-danger sm:text-sm";

export function TextInput(props: InputHTMLAttributes<HTMLInputElement>) {
  return <BaseInput {...props} className={`h-10 px-3 ${controlClass} ${props.className ?? ""}`} />;
}

export function TextArea(props: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      {...props}
      className={`min-h-28 px-3 py-2 font-mono text-xs! ${controlClass} ${props.className ?? ""}`}
    />
  );
}

export function Select(props: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <select
      {...props}
      className={`h-10 appearance-none bg-[length:16px] bg-[right_10px_center] bg-no-repeat pr-9 pl-3 ${controlClass} ${props.className ?? ""}`}
      style={{
        backgroundImage:
          "url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 20' fill='none' stroke='%239a948d' stroke-width='1.6' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='m6 8 4 4 4-4'/%3E%3C/svg%3E\")",
        ...props.style,
      }}
    />
  );
}

export function Field({
  label,
  hint,
  presets,
  children,
}: {
  label: string;
  hint?: ReactNode;
  /** One-click suggestions shown under the control, usually a <QuickPick>. */
  presets?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="grid content-start gap-1.5">
      <label className="grid gap-1.5">
        <span className="text-[13px] font-medium text-ink-soft">{label}</span>
        {children}
      </label>
      {presets}
      {hint ? <span className="text-[13px] text-muted">{hint}</span> : null}
    </div>
  );
}

export interface PickOption {
  /** Shown on the chip; looked up in the translations, so plain values pass through unchanged. */
  label: string;
  /** Applied when the chip is clicked. */
  value: string;
  /** Longer explanation, shown as a tooltip. */
  hint?: string;
}

/**
 * A row of one-click suggestions for a field. The chip whose value equals `value` is marked
 * as selected, so the row also tells the user which preset the field currently matches.
 */
export function QuickPick({
  label,
  options,
  value,
  onPick,
  size = "sm",
}: {
  /** Accessible name of the group, for example "Schedule presets". */
  label: string;
  options: readonly PickOption[];
  value?: string | null;
  onPick: (value: string, option: PickOption) => void;
  /** `md` is for chips that are the field's only control rather than a shortcut beside one. */
  size?: "sm" | "md";
}) {
  const t = useT();
  return (
    <div role="group" aria-label={label} className="flex flex-wrap items-center gap-1.5">
      {options.map((option) => {
        const selected = value != null && option.value === value;
        return (
          <button
            key={`${option.label}:${option.value}`}
            type="button"
            aria-pressed={selected}
            title={option.hint ? t(option.hint) : undefined}
            onClick={() => onPick(option.value, option)}
            className={`inline-flex items-center rounded-full border leading-none font-medium whitespace-nowrap transition-colors ${size === "md" ? "h-9 px-3.5 text-sm" : "h-7 px-2.5 text-[13px]"} focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-pine ${
              selected
                ? "border-pine-line bg-pine-soft text-pine"
                : "border-line-strong bg-surface text-ink-soft hover:bg-sunken hover:text-ink"
            }`}
          >
            {t(option.label)}
          </button>
        );
      })}
    </div>
  );
}

/** A setting with a few fixed choices, shown as chips so every option is visible at once. */
export function PickField({
  label,
  hint,
  options,
  value,
  onPick,
}: {
  label: string;
  hint?: ReactNode;
  options: readonly PickOption[];
  value: string;
  onPick: (value: string) => void;
}) {
  return (
    <div className="grid content-start gap-1.5">
      <span className="text-[13px] font-medium text-ink-soft">{label}</span>
      <QuickPick label={label} options={options} value={value} onPick={onPick} size="md" />
      {hint ? <span className="text-[13px] text-muted">{hint}</span> : null}
    </div>
  );
}

export function Card({
  title,
  description,
  actions,
  footer,
  flush = false,
  className = "",
  children,
}: {
  title?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  footer?: ReactNode;
  /** Drop body padding, for lists and tables that run edge to edge. */
  flush?: boolean;
  className?: string;
  children?: ReactNode;
}) {
  const hasHeader = title || description || actions;
  return (
    <section
      className={`overflow-hidden rounded-card border border-line bg-surface shadow-card ${className}`}
    >
      {hasHeader ? (
        <header className="flex flex-wrap items-start justify-between gap-x-4 gap-y-3 px-5 pt-5 sm:px-6">
          <div className="min-w-0">
            {title ? (
              <h2 className="text-[15px] leading-snug font-semibold tracking-[-0.005em] text-ink">
                {title}
              </h2>
            ) : null}
            {description ? <p className="mt-1 text-sm text-muted">{description}</p> : null}
          </div>
          {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
        </header>
      ) : null}
      {children ? (
        <div className={flush ? (hasHeader ? "pt-4" : "") : "px-5 py-5 sm:px-6"}>{children}</div>
      ) : null}
      {footer ? (
        <footer className="flex flex-wrap items-center justify-between gap-3 border-t border-line bg-paper/70 px-5 py-3 text-[13px] text-muted sm:px-6">
          {footer}
        </footer>
      ) : null}
    </section>
  );
}

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-end justify-between gap-x-6 gap-y-4">
      <div className="min-w-0">
        <h1 className="text-2xl leading-tight font-semibold tracking-[-0.015em] text-ink">
          {title}
        </h1>
        {description ? (
          <p className="mt-1.5 max-w-2xl text-[14.5px] text-muted">{description}</p>
        ) : null}
      </div>
      {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
    </div>
  );
}

export type Tone = "neutral" | "success" | "danger" | "warning" | "outline" | "live";

const toneClass: Record<Tone, string> = {
  neutral: "border-line bg-sunken text-ink-soft",
  success: "border-pine-line bg-pine-soft text-pine",
  danger: "border-danger-line bg-danger-soft text-danger",
  warning: "border-amber-line bg-amber-soft text-amber",
  outline: "border-line-strong bg-transparent text-muted",
  live: "border-line bg-sunken text-ink-soft",
};

const statusTones: Record<string, Tone> = {
  running: "success",
  succeeded: "success",
  ok: "success",
  online: "success",
  healthy: "success",
  active: "success",
  info: "neutral",
  queued: "live",
  deploying: "live",
  building: "live",
  pending: "live",
  partial: "warning",
  rolled_back: "warning",
  warning: "warning",
  outdated: "warning",
  failed: "danger",
  error: "danger",
  offline: "danger",
  firing: "danger",
  critical: "danger",
  stopped: "neutral",
  expired: "neutral",
};

/** Maps a status word to its badge tone. Unknown words are neutral. */
export function toneOf(status: string | null | undefined): Tone {
  return (status && statusTones[status]) || "neutral";
}

export function Badge({
  value,
  tone,
  dot,
}: {
  value: string;
  /** A tone name, or a status word whose tone to borrow. */
  tone?: string;
  dot?: boolean;
}) {
  const t = useT();
  const resolved: Tone = tone && tone in toneClass ? (tone as Tone) : toneOf(tone ?? value);
  const showDot = dot ?? (resolved !== "neutral" && resolved !== "outline");
  return (
    <span
      className={`inline-flex h-[22px] items-center gap-1.5 rounded-full border px-2 text-xs leading-none font-medium whitespace-nowrap ${toneClass[resolved]}`}
    >
      {showDot ? (
        <span
          className={`size-1.5 rounded-full bg-current ${resolved === "live" ? "animate-live" : ""}`}
        />
      ) : null}
      {t(value.replace(/_/g, " "))}
    </span>
  );
}

const dotClass: Record<Tone, string> = {
  neutral: "bg-faint",
  outline: "bg-faint",
  success: "bg-pine",
  danger: "bg-danger",
  warning: "bg-amber",
  live: "bg-ink-soft animate-live",
};

export function StatusDot({ status, className = "" }: { status: string; className?: string }) {
  const tone = toneOf(status);
  return (
    <span className={`relative inline-flex size-2 shrink-0 ${className}`} aria-hidden="true">
      {tone === "success" ? (
        <span className="absolute inset-0 rounded-full bg-pine opacity-30 motion-safe:animate-ping" />
      ) : null}
      <span className={`relative size-2 rounded-full ${dotClass[tone]}`} />
    </span>
  );
}

export function Alert({
  tone = "info",
  title,
  children,
  action,
}: {
  tone?: "info" | "success" | "warning" | "error";
  title?: ReactNode;
  children?: ReactNode;
  action?: ReactNode;
}) {
  const styles = {
    info: ["border-line bg-sunken/60 text-ink-soft", "info"],
    success: ["border-pine-line bg-pine-soft text-pine", "check"],
    warning: ["border-amber-line bg-amber-soft text-amber", "alert"],
    error: ["border-danger-line bg-danger-soft text-danger", "alert"],
  } as const;
  const [cls, icon] = styles[tone];
  return (
    <div
      role={tone === "error" ? "alert" : "status"}
      className={`flex animate-fade-up items-start gap-2.5 rounded-alert border px-3.5 py-3 text-sm leading-[1.45] ${cls}`}
    >
      <Icon name={icon} className="mt-0.5 shrink-0" />
      <div className="min-w-0 flex-1">
        {title ? <p className="mb-0.5 font-medium">{title}</p> : null}
        {children}
      </div>
      {action ? <div className="shrink-0 self-center">{action}</div> : null}
    </div>
  );
}

/** A thin usage bar. Turns amber from 75% and danger from 90%. */
export function Meter({
  value,
  label,
  className = "",
}: {
  value: number | null | undefined;
  label: string;
  className?: string;
}) {
  const pct = value == null ? null : Math.max(0, Math.min(100, value));
  const fill = pct == null ? "" : pct >= 90 ? "bg-danger" : pct >= 75 ? "bg-amber" : "bg-pine";
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={pct ?? undefined}
      className={`h-1.5 overflow-hidden rounded-full bg-sunken ${className}`}
    >
      {pct != null ? (
        <div
          className={`h-full rounded-full transition-[width] duration-500 ${fill}`}
          style={{ width: `${Math.max(pct, 2)}%` }}
        />
      ) : null}
    </div>
  );
}

export function Skeleton({ className = "" }: { className?: string }) {
  return <div className={`animate-pulse rounded-control bg-sunken ${className}`} />;
}

export function Avatar({ name, size = 32 }: { name: string; size?: number }) {
  const initials =
    name
      .split(/[\s@._-]+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((part) => part[0]?.toUpperCase())
      .join("") || "?";
  return (
    <span
      className="inline-flex shrink-0 items-center justify-center rounded-full border border-pine-line bg-pine-soft font-semibold text-pine"
      style={{ width: size, height: size, fontSize: size * 0.38 }}
      aria-hidden="true"
    >
      {initials}
    </span>
  );
}

export function Tabs<T extends string>({
  tabs,
  active,
  onChange,
}: {
  tabs: readonly (readonly [T, string])[];
  active: T;
  onChange: (tab: T) => void;
}) {
  const t = useT();
  return (
    <div role="tablist" className="flex gap-1 overflow-x-auto border-b border-line">
      {tabs.map(([key, label]) => (
        <button
          key={key}
          role="tab"
          type="button"
          aria-selected={active === key}
          className={`-mb-px border-b-2 px-3 py-2.5 text-sm whitespace-nowrap transition-colors ${active === key ? "border-pine font-medium text-ink" : "border-transparent text-muted hover:text-ink"}`}
          onClick={() => onChange(key)}
        >
          {t(label)}
        </button>
      ))}
    </div>
  );
}

/** Segmented control for a small set of filters. */
export function Segmented<T extends string>({
  options,
  value,
  onChange,
  label,
}: {
  options: readonly (readonly [T, string, number?])[];
  value: T;
  onChange: (value: T) => void;
  label: string;
}) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className="inline-flex rounded-control border border-line bg-sunken p-0.5"
    >
      {options.map(([key, text, count]) => (
        <button
          key={key}
          type="button"
          role="radio"
          aria-checked={value === key}
          onClick={() => onChange(key)}
          className={`inline-flex h-7 items-center gap-1.5 rounded-[5px] px-2.5 text-[13px] font-medium whitespace-nowrap transition-colors ${value === key ? "bg-surface text-ink shadow-card" : "text-muted hover:text-ink"}`}
        >
          {text}
          {count != null ? <span className="text-xs text-faint tabular-nums">{count}</span> : null}
        </button>
      ))}
    </div>
  );
}

export function Empty({
  children,
  icon,
  title,
  action,
}: {
  children?: ReactNode;
  icon?: IconName;
  title?: ReactNode;
  action?: ReactNode;
}) {
  if (!icon && !title && !action) {
    return <p className="text-sm text-muted">{children}</p>;
  }
  return (
    <div className="flex flex-col items-center px-6 py-10 text-center">
      {icon ? (
        <span className="mb-3 inline-flex size-10 items-center justify-center rounded-alert border border-pine-line bg-pine-soft text-pine">
          <Icon name={icon} size={20} />
        </span>
      ) : null}
      {title ? <p className="text-[15px] font-semibold text-ink">{title}</p> : null}
      {children ? <p className="mt-1 max-w-sm text-sm text-muted">{children}</p> : null}
      {action ? <div className="mt-4 flex flex-wrap justify-center gap-2">{action}</div> : null}
    </div>
  );
}

/** The icon that stands for a kind of service. */
export function kindIcon(kind: string): IconName {
  return kind === "database" ? "database" : kind === "compose" ? "layers" : "box";
}

/** A rounded tile holding a service kind's icon, for rows and page headers. */
export function KindTile({ kind, size = 36 }: { kind: string; size?: number }) {
  return (
    <span
      className="inline-flex shrink-0 items-center justify-center rounded-alert border border-line bg-sunken text-ink-soft"
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      <Icon name={kindIcon(kind)} size={Math.round(size * 0.5)} />
    </span>
  );
}

type StatTone = "neutral" | "success" | "warning" | "danger";

const statCaption: Record<StatTone, string> = {
  neutral: "text-muted",
  success: "font-medium text-pine",
  warning: "font-medium text-amber",
  danger: "font-medium text-danger",
};

const statChip: Record<StatTone, string> = {
  neutral: "bg-sunken text-ink-soft",
  success: "bg-pine-soft text-pine",
  warning: "bg-amber-soft text-amber",
  danger: "bg-danger-soft text-danger",
};

/** A headline number with a short caption; sits in a row of four at the top of a page. */
export function StatCard({
  icon,
  label,
  value,
  total,
  caption,
  tone = "neutral",
  loading = false,
}: {
  icon: IconName;
  label: string;
  value: ReactNode;
  /** When given, shows "of {total}" and a ratio bar for a numeric `value`. */
  total?: number;
  caption?: ReactNode;
  tone?: StatTone;
  loading?: boolean;
}) {
  const t = useT();
  const ratio = total && typeof value === "number" ? (value / total) * 100 : null;
  return (
    <div className="min-w-0 rounded-card border border-line bg-surface p-4 shadow-card sm:p-5">
      <div className="flex items-center gap-2 text-[13px] font-medium text-muted">
        <span
          className={`inline-flex size-7 shrink-0 items-center justify-center rounded-alert ${statChip[tone]}`}
        >
          <Icon name={icon} size={15} />
        </span>
        <span className="truncate">{label}</span>
      </div>
      {loading ? (
        <Skeleton className="mt-4 h-8 w-20" />
      ) : (
        <p className="mt-4 flex min-w-0 items-baseline gap-1.5 tabular-nums">
          <span className="truncate text-[28px] leading-none font-semibold tracking-[-0.02em] text-ink">
            {value}
          </span>
          {total != null ? (
            <span className="text-sm text-muted">{t("of {total}", { total })}</span>
          ) : null}
        </p>
      )}
      {ratio != null ? (
        <Meter
          value={loading ? null : ratio}
          label={t("{label} ratio", { label })}
          className="mt-3 [&>div]:bg-pine!"
        />
      ) : null}
      {caption ? (
        <p className={`mt-2.5 flex items-center gap-1.5 text-[13px] ${statCaption[tone]}`}>
          {tone === "danger" || tone === "warning" ? <Icon name="alert" size={14} /> : null}
          {loading ? " " : caption}
        </p>
      ) : null}
    </div>
  );
}

/** A search field sized for a table toolbar. */
export function SearchBox({
  value,
  onChange,
  placeholder,
  label,
  className = "",
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
  label: string;
  className?: string;
}) {
  return (
    <label className={`relative min-w-0 ${className}`}>
      <span className="sr-only">{label}</span>
      <Icon
        name="search"
        className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-faint"
      />
      <input
        type="search"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={placeholder}
        className="h-8 w-full rounded-control border border-line-strong bg-surface pr-3 pl-8 text-sm text-ink outline-none placeholder:text-faint focus:border-pine focus:shadow-[0_0_0_3px_color-mix(in_srgb,var(--pine)_14%,transparent)]"
      />
    </label>
  );
}

const cell = "first:pl-5 last:pr-5 sm:first:pl-6 sm:last:pr-6";

export function Th({ children, className = "" }: { children?: ReactNode; className?: string }) {
  return (
    <th
      scope="col"
      className={`px-3 py-2.5 text-left text-xs font-medium whitespace-nowrap text-muted ${cell} ${className}`}
    >
      {children}
    </th>
  );
}

export function Td({ children, className = "", ...props }: TdHTMLAttributes<HTMLTableCellElement>) {
  return (
    <td className={`px-3 py-3 align-middle ${cell} ${className}`} {...props}>
      {children}
    </td>
  );
}

/** An edge-to-edge table for use inside a flush Card. `head` is the header cells. */
export function DataTable({
  head,
  children,
  minWidth = 560,
}: {
  head: ReactNode;
  children: ReactNode;
  minWidth?: number;
}) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-left text-sm" style={{ minWidth }}>
        <thead className="border-y border-line bg-paper/60">
          <tr>{head}</tr>
        </thead>
        <tbody className="divide-y divide-line">{children}</tbody>
      </table>
    </div>
  );
}

/** Label and value pairs in a responsive grid. */
export function DefinitionList({
  items,
  columns = 2,
}: {
  items: readonly { label: string; value: ReactNode; mono?: boolean }[];
  columns?: 2 | 3;
}) {
  return (
    <dl className={`grid gap-x-8 gap-y-4 sm:grid-cols-2 ${columns === 3 ? "lg:grid-cols-3" : ""}`}>
      {items.map((item) => (
        <div key={item.label} className="min-w-0">
          <dt className="text-xs font-medium text-muted">{item.label}</dt>
          <dd
            className={`mt-1 text-sm break-words text-ink ${item.mono ? "font-mono text-[13px]" : ""}`}
          >
            {item.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}

export function CopyButton({ value, label }: { value: string; label?: string }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  return (
    <Button
      type="button"
      variant="ghost"
      size="sm"
      icon={copied ? "check" : "copy"}
      aria-label={label ?? t("Copy")}
      title={label ?? t("Copy")}
      onClick={() =>
        navigator.clipboard
          .writeText(value)
          .then(() => {
            toast.success(t("Copied"));
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          })
          .catch(() => toast.error(t("Copy failed; select the text instead")))
      }
    />
  );
}

/** Monospace text the user is likely to copy: URLs, keys, commands. */
export function CodeBlock({ children, copy }: { children: string; copy?: boolean }) {
  return (
    <div className="relative rounded-alert border border-line bg-paper">
      <pre
        className={`overflow-x-auto p-3 font-mono text-xs leading-relaxed break-all whitespace-pre-wrap text-ink-soft ${copy ? "pr-12" : ""}`}
      >
        {children}
      </pre>
      {copy ? (
        <span className="absolute top-1.5 right-1.5">
          <CopyButton value={children} />
        </span>
      ) : null}
    </div>
  );
}

/** A section that opens and closes; for advanced settings and long output. */
export function Disclosure({
  title,
  children,
  defaultOpen = false,
}: {
  title: ReactNode;
  children: ReactNode;
  defaultOpen?: boolean;
}) {
  return (
    <details
      open={defaultOpen}
      className="group rounded-alert border border-line bg-surface [&_summary::-webkit-details-marker]:hidden"
    >
      <summary className="flex cursor-pointer list-none items-center gap-2 rounded-alert px-3.5 py-2.5 text-sm font-medium text-ink-soft select-none hover:bg-paper">
        <Icon
          name="chevronRight"
          size={14}
          className="text-faint transition-transform group-open:rotate-90"
        />
        {title}
      </summary>
      <div className="border-t border-line px-3.5 py-4">{children}</div>
    </details>
  );
}

/** A modal dialog, or a drawer that slides in from the right for longer forms. */
export function Dialog({
  open,
  onClose,
  title,
  description,
  variant = "modal",
  wide = false,
  children,
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  description?: ReactNode;
  variant?: "modal" | "drawer";
  /** A wider drawer, for forms laid out in several columns. */
  wide?: boolean;
  children: ReactNode;
}) {
  const t = useT();
  const popup =
    variant === "drawer"
      ? `fixed inset-y-0 right-0 z-50 flex w-full ${wide ? "max-w-3xl" : "max-w-xl"} animate-slide-in flex-col border-l border-line bg-surface shadow-overlay`
      : "fixed top-1/2 left-1/2 z-50 flex max-h-[calc(100dvh-2rem)] w-[calc(100vw-2rem)] max-w-md -translate-x-1/2 -translate-y-1/2 animate-fade-up flex-col rounded-dialog border border-line bg-surface shadow-overlay";
  return (
    <BaseDialog.Root open={open} onOpenChange={(next) => !next && onClose()}>
      <BaseDialog.Portal>
        <BaseDialog.Backdrop className="fixed inset-0 z-50 bg-ink/40 backdrop-blur-[1px]" />
        <BaseDialog.Popup className={popup}>
          <header className="flex items-start justify-between gap-4 border-b border-line px-5 py-4 sm:px-6">
            <div className="min-w-0">
              <BaseDialog.Title className="text-base leading-snug font-semibold text-ink">
                {title}
              </BaseDialog.Title>
              {description ? (
                <BaseDialog.Description className="mt-1 text-sm text-muted">
                  {description}
                </BaseDialog.Description>
              ) : null}
            </div>
            <BaseDialog.Close
              aria-label={t("Close")}
              className="-mt-1 -mr-2 inline-flex size-8 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-sunken hover:text-ink"
            >
              <Icon name="close" />
            </BaseDialog.Close>
          </header>
          <div className="min-h-0 flex-1 overflow-y-auto px-5 py-5 sm:px-6">{children}</div>
        </BaseDialog.Popup>
      </BaseDialog.Portal>
    </BaseDialog.Root>
  );
}

export interface NavGroup<T extends string> {
  label: string;
  items: readonly { key: T; label: string; icon: IconName }[];
}

/**
 * Section navigation for a detail page: a grouped list beside the content on wide screens,
 * a scrolling row of pills above it on narrow ones.
 */
export function SectionNav<T extends string>({
  groups,
  active,
  onChange,
  label,
}: {
  groups: readonly NavGroup<T>[];
  active: T;
  onChange: (key: T) => void;
  label: string;
}) {
  const t = useT();
  return (
    <>
      <nav aria-label={label} className="hidden lg:block">
        <div className="sticky top-20 space-y-5">
          {groups.map((group) => (
            <div key={group.label}>
              <p className="px-2.5 pb-1.5 text-xs font-medium text-faint">{t(group.label)}</p>
              <ul className="space-y-0.5">
                {group.items.map((item) => (
                  <li key={item.key}>
                    <button
                      type="button"
                      aria-current={active === item.key ? "page" : undefined}
                      onClick={() => onChange(item.key)}
                      className={`group flex h-9 w-full items-center gap-2.5 rounded-control px-2.5 text-left text-sm transition-colors ${
                        active === item.key
                          ? "bg-sunken font-medium text-ink"
                          : "text-ink-soft hover:bg-sunken/60 hover:text-ink"
                      }`}
                    >
                      <Icon
                        name={item.icon}
                        className={
                          active === item.key ? "text-pine" : "text-faint group-hover:text-ink-soft"
                        }
                      />
                      {t(item.label)}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
      </nav>
      <nav aria-label={label} className="-mx-4 overflow-x-auto px-4 sm:-mx-6 sm:px-6 lg:hidden">
        <ul className="flex w-max gap-1.5 pb-1">
          {groups
            .flatMap((group) => group.items)
            .map((item) => (
              <li key={item.key}>
                <button
                  type="button"
                  aria-current={active === item.key ? "page" : undefined}
                  onClick={() => onChange(item.key)}
                  className={`inline-flex h-9 items-center gap-2 rounded-full border px-3.5 text-sm whitespace-nowrap transition-colors ${
                    active === item.key
                      ? "border-pine-line bg-pine-soft font-medium text-pine"
                      : "border-line-strong bg-surface text-ink-soft hover:bg-sunken"
                  }`}
                >
                  <Icon name={item.icon} size={15} />
                  {t(item.label)}
                </button>
              </li>
            ))}
        </ul>
      </nav>
    </>
  );
}

/** Console-style output for container logs and build logs; dark in both themes. */
export function LogPre({
  children,
  empty,
  className = "max-h-[32rem]",
}: {
  children?: string | null;
  empty: string;
  className?: string;
}) {
  return (
    <pre
      className={`overflow-auto rounded-alert border border-[#2c2a26] bg-[#12110f] p-3.5 font-mono text-xs leading-relaxed whitespace-pre-wrap text-[#d8d2c8] ${className}`}
    >
      {children || <span className="text-[#766f67]">{empty}</span>}
    </pre>
  );
}
