import { Button as BaseButton } from "@base-ui/react/button";
import { Input as BaseInput } from "@base-ui/react/input";
import type {
  ButtonHTMLAttributes,
  InputHTMLAttributes,
  ReactNode,
  SelectHTMLAttributes,
  TextareaHTMLAttributes,
} from "react";

const buttonClass =
  "inline-flex h-10 items-center justify-center rounded-md bg-accent px-3 text-sm font-medium text-accent-ink transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50";

const quietClass =
  "inline-flex h-10 items-center justify-center rounded-md border border-line bg-panel px-3 text-sm font-medium text-ink transition hover:bg-paper disabled:cursor-not-allowed disabled:opacity-50";

export function Button({
  quiet = false,
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { quiet?: boolean }) {
  return <BaseButton className={`${quiet ? quietClass : buttonClass} ${className}`} {...props} />;
}

export function TextInput(props: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <BaseInput
      {...props}
      className={`h-10 w-full rounded-md border border-line bg-paper px-3 text-sm text-ink outline-none focus:border-accent ${props.className ?? ""}`}
    />
  );
}

export function TextArea(props: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      {...props}
      className={`min-h-28 w-full rounded-md border border-line bg-paper px-3 py-2 font-mono text-xs text-ink outline-none focus:border-accent ${props.className ?? ""}`}
    />
  );
}

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="block space-y-1.5 text-sm">
      <span className="font-medium text-ink">{label}</span>
      {children}
    </label>
  );
}

export function Card({ title, children }: { title?: string; children: ReactNode }) {
  return (
    <section className="rounded-xl border border-line bg-panel p-5 shadow-sm">
      {title ? <h2 className="mb-4 text-lg font-semibold">{title}</h2> : null}
      {children}
    </section>
  );
}

export function Select(props: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <select
      {...props}
      className={`h-10 w-full rounded-md border border-line bg-paper px-3 text-sm text-ink ${props.className ?? ""}`}
    />
  );
}

const badgeTone: Record<string, string> = {
  running: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  succeeded: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  ok: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  queued: "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  deploying: "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  partial: "bg-amber-500/15 text-amber-700 dark:text-amber-300",
  stopped: "bg-zinc-500/15 text-muted",
  expired: "bg-zinc-500/15 text-muted",
  failed: "bg-red-500/15 text-red-700 dark:text-red-300",
};

export function Badge({ value, tone }: { value: string; tone?: string }) {
  return (
    <span
      className={`inline-flex rounded-full px-2 py-0.5 text-xs font-medium ${badgeTone[tone ?? value] ?? "bg-zinc-500/15 text-muted"}`}
    >
      {value}
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
          className={`-mb-px border-b-2 px-3 py-2 text-sm whitespace-nowrap ${active === key ? "border-accent font-medium text-ink" : "border-transparent text-muted"}`}
          onClick={() => onChange(key)}
        >
          {label}
        </button>
      ))}
    </div>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <p className="text-sm text-muted">{children}</p>;
}
