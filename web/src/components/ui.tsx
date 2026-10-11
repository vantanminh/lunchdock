import { Button as BaseButton } from "@base-ui/react/button";
import { Input as BaseInput } from "@base-ui/react/input";
import type {
  ButtonHTMLAttributes,
  InputHTMLAttributes,
  ReactNode,
  SelectHTMLAttributes,
  TextareaHTMLAttributes,
} from "react";
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
  children,
}: {
  label: string;
  hint?: ReactNode;
  children: ReactNode;
}) {
  return (
    <label className="grid gap-1.5">
      <span className="text-[13px] font-medium text-ink-soft">{label}</span>
      {children}
      {hint ? <span className="text-[13px] text-muted">{hint}</span> : null}
    </label>
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
  const resolved: Tone =
    tone && tone in toneClass ? (tone as Tone) : toneOf(tone ?? value);
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
  const fill =
    pct == null ? "" : pct >= 90 ? "bg-danger" : pct >= 75 ? "bg-amber" : "bg-pine";
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
          {label}
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
          {count != null ? (
            <span className="text-xs text-faint tabular-nums">{count}</span>
          ) : null}
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
