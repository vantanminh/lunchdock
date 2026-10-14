import { Dialog as BaseDialog } from "@base-ui/react/dialog";
import { useQuery } from "@tanstack/react-query";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { useNavigate } from "react-router-dom";
import { api, getConnection, subscribeConnection } from "../api";
import { useT } from "../i18n";
import { NAV_GROUPS } from "../layout/nav";
import { setDarkTheme, useDarkTheme } from "../theme";
import type { Schemas } from "../types";
import { Icon, type IconName } from "./icons";
import { kindIcon } from "./ui";

interface Command {
  id: string;
  label: string;
  hint: string;
  group: string;
  icon: IconName;
  run: () => void;
}

/** Lower case without Vietnamese diacritics, so "trien khai" finds "Triển khai". */
function fold(text: string): string {
  return text
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/đ/g, "d")
    .replace(/Đ/g, "d")
    .toLowerCase();
}

/** Whether every word of the query appears in the command, best matches first. */
function score(command: Command, words: string[]): number {
  if (words.length === 0) {
    return 1;
  }
  const label = fold(command.label);
  const all = `${label} ${fold(command.hint)} ${fold(command.group)}`;
  if (!words.every((word) => all.includes(word))) {
    return 0;
  }
  const first = words[0] ?? "";
  return label.startsWith(first) ? 3 : label.includes(first) ? 2 : 1;
}

/** Open the palette with Ctrl+K, ⌘K, or "/" outside a text field. */
export function usePaletteShortcut(onOpen: () => void) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing =
        target?.isContentEditable ||
        target?.tagName === "INPUT" ||
        target?.tagName === "TEXTAREA" ||
        target?.tagName === "SELECT";
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        onOpen();
      } else if (event.key === "/" && !typing && !event.metaKey && !event.ctrlKey) {
        event.preventDefault();
        onOpen();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onOpen]);
}

export function isMac(): boolean {
  return /mac|iphone|ipad/i.test(navigator.platform || navigator.userAgent);
}

/** Jump to any page or service, or run a common action, from the keyboard. */
export function CommandPalette({ open, onClose }: { open: boolean; onClose: () => void }) {
  const t = useT();
  const navigate = useNavigate();
  const dark = useDarkTheme();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const list = useRef<HTMLUListElement>(null);
  const services = useQuery({
    queryKey: ["services"],
    queryFn: () => api<Schemas["ServiceSummary"][]>("/api/v1/services"),
    enabled: open,
    staleTime: 30_000,
  });

  useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
    }
  }, [open]);

  const commands = useMemo<Command[]>(() => {
    const go = (to: string) => () => {
      onClose();
      navigate(to);
    };
    const pages: Command[] = NAV_GROUPS.flatMap((group) =>
      group.items.map(([to, label, icon]) => ({
        id: `page:${to}`,
        label: t(label),
        hint: t(group.label),
        group: t("Pages"),
        icon,
        run: go(to),
      })),
    );
    pages.push({
      id: "page:/account",
      label: t("Account"),
      hint: t("Password, two-factor and language"),
      group: t("Pages"),
      icon: "user",
      run: go("/account"),
    });
    const actions: Command[] = [
      {
        id: "action:project",
        label: t("New project"),
        hint: t("Projects"),
        group: t("Actions"),
        icon: "plus",
        run: go("/projects"),
      },
      {
        id: "action:template",
        label: t("Deploy a template"),
        hint: t("Templates"),
        group: t("Actions"),
        icon: "layers",
        run: go("/templates"),
      },
      {
        id: "action:load",
        label: t("Pause or cancel builds"),
        hint: t("Load"),
        group: t("Actions"),
        icon: "pause",
        run: go("/load"),
      },
      {
        id: "action:theme",
        label: dark ? t("Use light theme") : t("Use dark theme"),
        hint: t("Appearance"),
        group: t("Actions"),
        icon: dark ? "sun" : "moon",
        run: () => {
          setDarkTheme(!dark);
          onClose();
        },
      },
    ];
    const serviceCommands: Command[] = (services.data ?? []).map((service) => ({
      id: `service:${service.id}`,
      label: service.name,
      hint: `${service.project_name} / ${service.environment_name}`,
      group: t("Services"),
      icon: kindIcon(service.kind),
      run: go(`/services/${service.id}`),
    }));
    return [...actions, ...pages, ...serviceCommands];
  }, [t, navigate, onClose, dark, services.data]);

  const results = useMemo(() => {
    const words = fold(query).split(/\s+/).filter(Boolean);
    return commands
      .map((command, index) => ({ command, rank: score(command, words), index }))
      .filter((entry) => entry.rank > 0)
      .sort((a, b) => b.rank - a.rank || a.index - b.index)
      .slice(0, 40)
      .map((entry) => entry.command);
  }, [commands, query]);

  useEffect(() => {
    setActive(0);
  }, [query]);

  useEffect(() => {
    list.current
      ?.querySelector<HTMLElement>(`[data-index="${active}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((index) => Math.min(index + 1, Math.max(results.length - 1, 0)));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((index) => Math.max(index - 1, 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      results[active]?.run();
    }
  };

  let lastGroup = "";
  return (
    <BaseDialog.Root open={open} onOpenChange={(next) => !next && onClose()}>
      <BaseDialog.Portal>
        <BaseDialog.Backdrop className="fixed inset-0 z-50 bg-black/40 backdrop-blur-[1px]" />
        <BaseDialog.Popup className="fixed top-[12vh] left-1/2 z-50 flex max-h-[70vh] w-[calc(100vw-2rem)] max-w-xl -translate-x-1/2 animate-fade-up flex-col overflow-hidden rounded-dialog border border-line bg-surface shadow-overlay">
          <BaseDialog.Title className="sr-only">{t("Search and jump")}</BaseDialog.Title>
          <div className="flex items-center gap-2.5 border-b border-line px-4">
            <Icon name="search" size={18} className="shrink-0 text-faint" />
            <input
              autoFocus
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={onKeyDown}
              placeholder={t("Search pages, services and actions…")}
              aria-label={t("Search pages, services and actions")}
              role="combobox"
              aria-expanded="true"
              aria-controls="command-results"
              aria-activedescendant={results[active] ? `command-${active}` : undefined}
              className="h-13 min-w-0 flex-1 bg-transparent text-[15px] text-ink outline-none placeholder:text-faint"
            />
            <kbd className="hidden rounded-sm border border-line px-1.5 py-0.5 text-[11px] text-muted sm:inline">
              Esc
            </kbd>
          </div>
          <ul
            ref={list}
            id="command-results"
            role="listbox"
            aria-label={t("Results")}
            className="min-h-0 flex-1 overflow-y-auto p-2"
          >
            {results.length === 0 ? (
              <li className="px-3 py-8 text-center text-sm text-muted">
                {services.isLoading ? t("Loading…") : t("Nothing matches “{query}”", { query })}
              </li>
            ) : (
              results.map((command, index) => {
                const header = command.group !== lastGroup ? command.group : null;
                lastGroup = command.group;
                return (
                  <li key={command.id} role="presentation">
                    {header ? (
                      <p className="px-3 pt-2.5 pb-1 text-xs font-medium text-faint">{header}</p>
                    ) : null}
                    <div
                      id={`command-${index}`}
                      data-index={index}
                      role="option"
                      aria-selected={index === active}
                      onMouseMove={() => setActive(index)}
                      onClick={() => command.run()}
                      className={`flex cursor-pointer items-center gap-3 rounded-control px-3 py-2 text-sm ${index === active ? "bg-sunken text-ink" : "text-ink-soft"}`}
                    >
                      <Icon
                        name={command.icon}
                        className={index === active ? "text-pine" : "text-faint"}
                      />
                      <span className="min-w-0 flex-1 truncate font-medium">{command.label}</span>
                      <span className="max-w-[45%] truncate text-xs text-muted">{command.hint}</span>
                      {index === active ? (
                        <Icon name="arrowRight" size={14} className="shrink-0 text-faint" />
                      ) : null}
                    </div>
                  </li>
                );
              })
            )}
          </ul>
          <footer className="flex items-center gap-4 border-t border-line bg-paper/70 px-4 py-2 text-xs text-muted">
            <span>
              <kbd className="font-sans">↑↓</kbd> {t("to move")}
            </span>
            <span>
              <kbd className="font-sans">↵</kbd> {t("to open")}
            </span>
          </footer>
        </BaseDialog.Popup>
      </BaseDialog.Portal>
    </BaseDialog.Root>
  );
}

/** The header button that opens the palette. */
export function PaletteButton({ onOpen }: { onOpen: () => void }) {
  const t = useT();
  const shortcut = isMac() ? "⌘K" : "Ctrl K";
  return (
    <button
      type="button"
      onClick={onOpen}
      aria-label={t("Search and jump")}
      aria-keyshortcuts="Control+K Meta+K"
      className="inline-flex h-9 items-center gap-2 rounded-control border border-line bg-surface px-2.5 text-sm text-muted shadow-card transition-colors hover:border-line-strong hover:text-ink sm:w-64"
    >
      <Icon name="search" size={16} className="shrink-0" />
      <span className="hidden flex-1 truncate text-left sm:inline">{t("Search or jump to…")}</span>
      <kbd className="hidden rounded-sm border border-line bg-paper px-1.5 py-0.5 font-sans text-[11px] text-faint sm:inline">
        {shortcut}
      </kbd>
    </button>
  );
}

/** How the panel answered lately: live, slow, busy, or not at all. */
export function ConnectionStatus() {
  const t = useT();
  const connection = useSyncExternalStore(subscribeConnection, getConnection);
  const styles = {
    ok: ["bg-pine", "text-muted", t("Live")],
    slow: ["bg-amber", "text-amber", t("Slow")],
    busy: ["bg-amber", "text-amber", t("Busy")],
    down: ["bg-danger animate-live", "text-danger", t("Reconnecting…")],
  } as const;
  const [dot, text, label] = styles[connection.state];
  const title =
    connection.state === "down"
      ? t("The panel did not answer. Retrying…")
      : connection.state === "busy"
        ? t("The panel asked to retry in a few seconds.")
        : connection.latencyMs != null
          ? t("The panel answered in {ms} ms", { ms: connection.latencyMs })
          : t("Connected");
  return (
    <span
      role="status"
      title={title}
      className={`inline-flex h-9 items-center gap-2 rounded-control px-2 text-xs font-medium ${text}`}
    >
      <span className={`size-2 rounded-full ${dot}`} aria-hidden="true" />
      <span className={connection.state === "ok" ? "hidden md:inline" : ""}>{label}</span>
    </span>
  );
}
