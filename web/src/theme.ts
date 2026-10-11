import { useSyncExternalStore } from "react";

const root = document.documentElement;

function subscribe(onChange: () => void) {
  const observer = new MutationObserver(onChange);
  observer.observe(root, { attributes: true, attributeFilter: ["class"] });
  return () => observer.disconnect();
}

/** True while the dark theme class is on <html>; re-renders when the theme is toggled. */
export function useDarkTheme(): boolean {
  return useSyncExternalStore(subscribe, () => root.classList.contains("dark"));
}

export function setDarkTheme(dark: boolean) {
  root.classList.toggle("dark", dark);
  try {
    localStorage.setItem("rudder_theme", dark ? "dark" : "light");
  } catch {
    // Storage can be blocked; the theme still applies for this session.
  }
}
