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

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  const method = (init.method ?? "GET").toUpperCase();
  if (init.body && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  if (csrfToken && method !== "GET" && method !== "HEAD") {
    headers.set("x-csrf-token", csrfToken);
  }
  const response = await fetch(path, { ...init, headers, credentials: "same-origin" });
  if (response.status === 204) {
    return undefined as T;
  }
  const text = await response.text();
  const data = text ? (JSON.parse(text) as unknown) : null;
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
