const CSRF_KEY = "rudder_csrf";

let csrfToken = sessionStorage.getItem(CSRF_KEY);

export function setCsrf(token: string | null) {
  csrfToken = token;
  if (token) {
    sessionStorage.setItem(CSRF_KEY, token);
  } else {
    sessionStorage.removeItem(CSRF_KEY);
  }
}

export class ApiError extends Error {
  status: number;
  code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

/**
 * How the panel answered lately: `ok`, `slow` (a read took longer than {@link SLOW_MS}),
 * `busy` (the panel turned a request away and asked to retry), or `down` (no answer, or
 * the proxy in front of it could not reach it).
 */
export type ConnectionState = "ok" | "slow" | "busy" | "down";

export interface Connection {
  state: ConnectionState;
  /** Milliseconds the last read took. */
  latencyMs: number | null;
  /** When the last answer, or the last failure, came in. */
  at: number;
}

const SLOW_MS = 2500;
let connection: Connection = { state: "ok", latencyMs: null, at: 0 };
const connectionListeners = new Set<() => void>();

function report(state: ConnectionState, latencyMs: number | null) {
  if (state === connection.state && latencyMs === connection.latencyMs) {
    return;
  }
  connection = { state, latencyMs: latencyMs ?? connection.latencyMs, at: Date.now() };
  connectionListeners.forEach((listener) => listener());
}

export function subscribeConnection(listener: () => void): () => void {
  connectionListeners.add(listener);
  return () => {
    connectionListeners.delete(listener);
  };
}

export function getConnection(): Connection {
  return connection;
}

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  const method = (init.method ?? "GET").toUpperCase();
  if (init.body && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  if (csrfToken && method !== "GET" && method !== "HEAD") {
    headers.set("x-csrf-token", csrfToken);
  }
  const started = performance.now();
  let response: Response;
  try {
    response = await fetch(path, { ...init, headers, credentials: "same-origin" });
  } catch (error) {
    if (!init.signal?.aborted) {
      report("down", null);
    }
    throw error;
  }
  const elapsed = Math.round(performance.now() - started);
  if (response.status === 502 || response.status === 504) {
    report("down", null);
  } else if (response.status === 503) {
    report("busy", null);
  } else if (method === "GET") {
    report(elapsed > SLOW_MS ? "slow" : "ok", elapsed);
  } else if (connection.state === "down" || connection.state === "busy") {
    report("ok", null);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  const text = await response.text();
  let data: unknown = null;
  try {
    data = text ? (JSON.parse(text) as unknown) : null;
  } catch {
    // A proxy in front of the panel (Traefik's "Bad Gateway") answers in plain text.
    if (response.ok) {
      throw new ApiError(response.status, "invalid_response", "The panel sent an unreadable answer.");
    }
  }
  if (!response.ok) {
    const payload = data as { error?: { code?: string; message?: string } } | null;
    throw new ApiError(
      response.status,
      payload?.error?.code ?? "error",
      payload?.error?.message ?? response.statusText,
    );
  }
  return data as T;
}

/** Fetch a file from the API and hand it to the browser as a download. */
export async function download(path: string, init: RequestInit = {}): Promise<void> {
  const headers = new Headers(init.headers);
  const method = (init.method ?? "GET").toUpperCase();
  if (init.body && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  if (csrfToken && method !== "GET" && method !== "HEAD") {
    headers.set("x-csrf-token", csrfToken);
  }
  const response = await fetch(path, { ...init, headers, credentials: "same-origin" });
  if (!response.ok) {
    const payload = (await response.json().catch(() => null)) as {
      error?: { code?: string; message?: string };
    } | null;
    throw new ApiError(
      response.status,
      payload?.error?.code ?? "error",
      payload?.error?.message ?? response.statusText,
    );
  }
  const disposition = response.headers.get("content-disposition") ?? "";
  const name = /filename="([^"]+)"/.exec(disposition)?.[1] ?? "download";
  const url = URL.createObjectURL(await response.blob());
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}
