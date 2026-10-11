export type Role = "admin" | "viewer";

export interface User {
  id: string;
  email: string;
  display_name: string;
  role: Role;
  totp_enabled: boolean;
}

export interface SessionResponse {
  user: User;
  csrf_token: string | null;
}

export interface TokenView {
  id: string;
  name: string;
  scopes: string[];
  expires_at: string | null;
  last_used_at: string | null;
  created_at: string;
}

export interface CreatedToken extends TokenView {
  token: string;
}

export interface AuditEvent {
  id: number;
  actor_user_id: string | null;
  action: string;
  resource_type: string;
  resource_id: string | null;
  metadata: Record<string, unknown>;
  ip: string | null;
  created_at: string;
}
