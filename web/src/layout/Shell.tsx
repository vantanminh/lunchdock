import { Suspense, useCallback, useEffect, useState } from "react";
import { Link, NavLink, Outlet, useLocation } from "react-router-dom";
import { api, setCsrf } from "../api";
import { useAuth } from "../auth";
import { Icon, KnotreeMark } from "../components/icons";
import {
  CommandPalette,
  ConnectionStatus,
  PaletteButton,
  usePaletteShortcut,
} from "../components/command-palette";
import { InsecureBanner } from "../components/insecure-banner";
import { LoadBanner } from "../components/load";
import { Avatar, Skeleton } from "../components/ui";
import { msg, useT } from "../i18n";
import { setDarkTheme, useDarkTheme } from "../theme";
import { NAV_GROUPS } from "./nav";

const allItems = [...NAV_GROUPS.flatMap((group) => group.items.map((item) => ({ group, item })))];

function currentPage(pathname: string) {
  if (pathname.startsWith("/services/")) {
    return { group: msg("Deploy"), label: msg("Service") };
  }
  if (pathname === "/account") {
    return { group: msg("You"), label: msg("Account") };
  }
  const match = allItems.find(({ item: [to] }) =>
    to === "/" ? pathname === "/" : pathname.startsWith(to),
  );
  return match ? { group: match.group.label, label: match.item[1] } : null;
}

export function Shell() {
  const t = useT();
  const { pathname } = useLocation();
  const [open, setOpen] = useState(false);
  const [palette, setPalette] = useState(false);
  const page = currentPage(pathname);
  const openPalette = useCallback(() => setPalette(true), []);
  const closePalette = useCallback(() => setPalette(false), []);
  usePaletteShortcut(openPalette);

  useEffect(() => {
    setOpen(false);
  }, [pathname]);

  useEffect(() => {
    document.title = page ? `${t(page.label)} · Rudder` : "Rudder";
  }, [page, t]);

  useEffect(() => {
    if (!open) {
      return;
    }
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && setOpen(false);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  return (
    <div className="min-h-screen lg:grid lg:grid-cols-[256px_minmax(0,1fr)]">
      <a
        href="#main"
        className="sr-only z-50 rounded-control bg-surface px-3 py-2 focus:not-sr-only focus:fixed focus:top-3 focus:left-3"
      >
        {t("Skip to content")}
      </a>

      <div className="hidden border-r border-line bg-surface lg:block">
        <aside className="sticky top-0 h-screen">
          <Sidebar />
        </aside>
      </div>

      {open ? (
        <div className="fixed inset-0 z-40 lg:hidden">
          <button
            type="button"
            aria-label={t("Close menu")}
            className="absolute inset-0 bg-ink/30 backdrop-blur-[1px]"
            onClick={() => setOpen(false)}
          />
          <aside className="absolute inset-y-0 left-0 w-[280px] max-w-[85vw] animate-fade-up border-r border-line bg-surface shadow-overlay">
            <Sidebar onClose={() => setOpen(false)} />
          </aside>
        </div>
      ) : null}

      <div className="flex min-h-screen min-w-0 flex-col">
        <InsecureBanner />
        <LoadBanner />
        <header className="sticky top-0 z-30 flex h-14 items-center gap-3 border-b border-line bg-paper/85 px-4 backdrop-blur-md sm:px-6 lg:px-8">
          <button
            type="button"
            className="-ml-1 inline-flex size-9 items-center justify-center rounded-control text-ink-soft hover:bg-sunken lg:hidden"
            aria-label={t("Open menu")}
            aria-expanded={open}
            onClick={() => setOpen(true)}
          >
            <Icon name="menu" size={20} />
          </button>
          <Link
            to="/"
            className="flex items-center gap-2 lg:hidden"
            aria-label={t("Rudder overview")}
          >
            <KnotreeMark size={24} />
            <span className="text-[15px] font-semibold tracking-[-0.01em]">Rudder</span>
          </Link>
          {page ? (
            <nav
              aria-label={t("Breadcrumb")}
              className="hidden items-center gap-1.5 text-sm lg:flex"
            >
              <span className="text-muted">{t(page.group)}</span>
              <Icon name="chevronRight" size={14} className="text-faint" />
              <span className="font-medium text-ink">{t(page.label)}</span>
            </nav>
          ) : null}
          <div className="ml-auto flex items-center gap-1.5">
            <PaletteButton onOpen={openPalette} />
            <ConnectionStatus />
            <ThemeToggle />
          </div>
        </header>
        <CommandPalette open={palette} onClose={closePalette} />
        <main
          id="main"
          className="mx-auto w-full max-w-6xl flex-1 space-y-6 px-4 py-6 sm:px-6 sm:py-8 lg:px-8"
        >
          <Suspense fallback={<PageSkeleton />}>
            <Outlet />
          </Suspense>
        </main>
      </div>
    </div>
  );
}

function Sidebar({ onClose }: { onClose?: () => void }) {
  const t = useT();
  return (
    <div className="flex h-full flex-col">
      <div className="flex h-14 shrink-0 items-center justify-between border-b border-line px-4">
        <Link to="/" className="flex items-center gap-2.5" aria-label={t("Rudder overview")}>
          <KnotreeMark size={26} />
          <span className="text-[15px] font-semibold tracking-[-0.01em] text-ink">Knotree</span>
          <span className="border-l border-line-strong pl-2.5 text-[15px] text-muted">Rudder</span>
        </Link>
        {onClose ? (
          <button
            type="button"
            aria-label={t("Close menu")}
            className="inline-flex size-8 items-center justify-center rounded-control text-muted hover:bg-sunken hover:text-ink"
            onClick={onClose}
          >
            <Icon name="close" />
          </button>
        ) : null}
      </div>
      <nav aria-label={t("Main")} className="flex-1 space-y-5 overflow-y-auto px-3 py-4">
        {NAV_GROUPS.map((group) => (
          <div key={group.label}>
            <p className="px-2.5 pb-1.5 text-xs font-medium text-faint">{t(group.label)}</p>
            <ul className="space-y-0.5">
              {group.items.map(([to, label, icon]) => (
                <li key={to}>
                  <NavLink
                    to={to}
                    end={to === "/"}
                    className={({ isActive }) =>
                      `group flex h-9 items-center gap-2.5 rounded-control px-2.5 text-sm transition-colors ${
                        isActive
                          ? "bg-sunken font-medium text-ink"
                          : "text-ink-soft hover:bg-sunken/60 hover:text-ink"
                      }`
                    }
                  >
                    {({ isActive }) => (
                      <>
                        <Icon
                          name={icon}
                          className={
                            isActive ? "text-pine" : "text-faint group-hover:text-ink-soft"
                          }
                        />
                        {t(label)}
                      </>
                    )}
                  </NavLink>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </nav>
      <UserCard />
    </div>
  );
}

function UserCard() {
  const t = useT();
  const { user, refresh } = useAuth();
  const [pending, setPending] = useState(false);

  async function logout() {
    setPending(true);
    try {
      await api("/api/v1/logout", { method: "POST" });
      setCsrf(null);
      await refresh();
    } finally {
      setPending(false);
    }
  }

  const name = user?.display_name || user?.email || "";
  return (
    <div className="flex shrink-0 items-center gap-2 border-t border-line p-3">
      <NavLink
        to="/account"
        className={({ isActive }) =>
          `flex min-w-0 flex-1 items-center gap-2.5 rounded-control p-1.5 transition-colors hover:bg-sunken ${isActive ? "bg-sunken" : ""}`
        }
      >
        <Avatar name={name} size={32} />
        <span className="min-w-0 leading-tight">
          <span className="block truncate text-sm font-medium text-ink">{name}</span>
          <span className="block truncate text-xs text-muted first-letter:uppercase">
            {user?.role ? t(user.role) : null}
          </span>
        </span>
      </NavLink>
      <button
        type="button"
        aria-label={t("Sign out")}
        title={t("Sign out")}
        disabled={pending}
        onClick={() => void logout()}
        className="inline-flex size-9 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-sunken hover:text-ink disabled:opacity-55"
      >
        <Icon name="logout" />
      </button>
    </div>
  );
}

function ThemeToggle() {
  const t = useT();
  const dark = useDarkTheme();
  const label = dark ? t("Use light theme") : t("Use dark theme");
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className="inline-flex size-9 items-center justify-center rounded-control text-ink-soft transition-colors hover:bg-sunken hover:text-ink"
      onClick={() => setDarkTheme(!dark)}
    >
      <Icon name={dark ? "sun" : "moon"} size={18} />
    </button>
  );
}

function PageSkeleton() {
  const t = useT();
  return (
    <div className="space-y-6" aria-busy="true" aria-label={t("Loading")}>
      <div className="space-y-2">
        <Skeleton className="h-7 w-48" />
        <Skeleton className="h-4 w-80 max-w-full" />
      </div>
      <Skeleton className="h-48 w-full rounded-card" />
      <Skeleton className="h-32 w-full rounded-card" />
    </div>
  );
}
