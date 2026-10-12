import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef, useState } from "react";
import { useAuth } from "../auth";
import { Alert, Button, Empty, Select, StatusDot } from "./ui";
import { useT } from "../i18n";

type Status = "idle" | "connecting" | "open" | "closed";

/** Interactive shell in a service container, over the panel's terminal WebSocket. */
export function TerminalPanel({
  serviceId,
  containers,
}: {
  serviceId: string;
  /** Compose service names; empty for single-container services. */
  containers: string[];
}) {
  const t = useT();
  const { user } = useAuth();
  const host = useRef<HTMLDivElement | null>(null);
  const [container, setContainer] = useState("");
  const [status, setStatus] = useState<Status>("idle");
  const [note, setNote] = useState<string | null>(null);
  const [session, setSession] = useState(0);

  useEffect(() => {
    if (session === 0 || !host.current) return;
    const term = new Terminal({
      cursorBlink: true,
      fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
      fontSize: 13,
      theme: { background: "#0b0d10" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host.current);
    fit.fit();
    const params = new URLSearchParams({ cols: String(term.cols), rows: String(term.rows) });
    if (container) params.set("container", container);
    const scheme = window.location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(
      `${scheme}://${window.location.host}/api/v1/services/${serviceId}/terminal?${params}`,
    );
    ws.binaryType = "arraybuffer";
    setStatus("connecting");
    setNote(null);
    ws.onopen = () => {
      setStatus("open");
      term.focus();
    };
    ws.onmessage = (event) => {
      if (event.data instanceof ArrayBuffer) {
        term.write(new Uint8Array(event.data));
        return;
      }
      const message = JSON.parse(String(event.data)) as { type: string; message?: string };
      if (message.type === "exit") setNote("The shell exited.");
      if (message.type === "error") setNote(message.message ?? "Terminal error");
    };
    ws.onclose = () => setStatus("closed");
    ws.onerror = () => setNote("Could not open a terminal. Is the container running?");
    const input = term.onData((data) => {
      if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: "input", data }));
    });
    const resize = term.onResize(({ cols, rows }) => {
      if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: "resize", cols, rows }));
    });
    const observer = new ResizeObserver(() => fit.fit());
    observer.observe(host.current);
    return () => {
      observer.disconnect();
      input.dispose();
      resize.dispose();
      ws.close();
      term.dispose();
    };
  }, [session, serviceId, container]);

  if (user?.role !== "admin") {
    return (
      <Empty icon="lock" title={t("Admins only")}>
        {t("Only admins can open a terminal.")}
      </Empty>
    );
  }
  const needsContainer = containers.length > 0 && !container;
  const live = status === "open" || status === "connecting";

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        {containers.length > 0 ? (
          <div className="w-56 max-w-full">
            <Select
              value={container}
              aria-label={t("Compose service")}
              onChange={(event) => {
                setContainer(event.target.value);
                setSession(0);
                setStatus("idle");
              }}
            >
              <option value="">{t("Choose a service")}</option>
              {containers.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </Select>
          </div>
        ) : null}
        <Button
          icon="terminal"
          disabled={needsContainer}
          onClick={() => setSession((value) => value + 1)}
        >
          {live ? t("Reconnect") : t("Open shell")}
        </Button>
        {status === "open" ? (
          <Button variant="secondary" onClick={() => setSession(0)}>
            {t("Disconnect")}
          </Button>
        ) : null}
        <span className="ml-auto inline-flex items-center gap-2 text-[13px] text-muted">
          {status === "open" || status === "connecting" || status === "closed" ? (
            <StatusDot
              status={status === "open" ? "running" : status === "connecting" ? "building" : "stopped"}
            />
          ) : null}
          {status === "open" ? t("Connected") : status === "connecting" ? t("Connecting…") : ""}
          {status === "closed" ? t("Disconnected") : ""}
        </span>
      </div>
      {note ? <Alert tone="warning">{note}</Alert> : null}
      {session === 0 ? (
        <div className="flex h-40 items-center justify-center rounded-alert border border-dashed border-line-strong px-4 text-center text-sm text-muted">
          {needsContainer ? t("Choose a service, then open a shell.") : t("No shell open yet.")}
        </div>
      ) : null}
      <div
        ref={host}
        className={`h-[28rem] overflow-hidden rounded-alert border border-[#2c2a26] bg-[#0b0d10] p-2 ${session === 0 ? "hidden" : ""}`}
      />
      <p className="text-xs text-muted">
        {t(
          "Sessions run the container's bash or sh as its default user and are recorded in the audit log.",
        )}
      </p>
    </div>
  );
}
