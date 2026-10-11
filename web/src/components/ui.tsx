import { Button as BaseButton } from "@base-ui/react/button";
import { Input as BaseInput } from "@base-ui/react/input";
import type { ButtonHTMLAttributes, InputHTMLAttributes, ReactNode } from "react";

const buttonClass =
  "inline-flex h-10 items-center justify-center rounded-md bg-accent px-3 text-sm font-medium text-accent-ink transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50";

const quietClass =
  "inline-flex h-10 items-center justify-center rounded-md border border-line bg-panel px-3 text-sm font-medium text-ink transition hover:bg-paper disabled:cursor-not-allowed disabled:opacity-50";

export function Button({
  quiet = false,
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { quiet?: boolean }) {
  return (
    <BaseButton className={`${quiet ? quietClass : buttonClass} ${className}`} {...props} />
  );
}

export function TextInput(props: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <BaseInput
      {...props}
      className={`h-10 w-full rounded-md border border-line bg-paper px-3 text-sm text-ink outline-none focus:border-accent ${props.className ?? ""}`}
    />
  );
}

export function Field({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <label className="block space-y-1.5 text-sm">
      <span className="font-medium text-ink">{label}</span>
      {children}
    </label>
  );
}

export function Card({
  title,
  children,
}: {
  title?: string;
  children: ReactNode;
}) {
  return (
    <section className="rounded-xl border border-line bg-panel p-5 shadow-sm">
      {title ? <h2 className="mb-4 text-lg font-semibold">{title}</h2> : null}
      {children}
    </section>
  );
}
