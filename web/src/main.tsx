import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./index.css";

let stored: string | null = null;
try {
  stored = localStorage.getItem("rudder_theme");
} catch {
  // Storage can be blocked; fall back to the system preference.
}
const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
document.documentElement.classList.toggle("dark", stored ? stored === "dark" : prefersDark);

const root = document.getElementById("root");
if (!root) {
  throw new Error("root element missing");
}

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
