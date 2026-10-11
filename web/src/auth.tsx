import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createContext, useContext, type ReactNode } from "react";
import { api, ApiError, setCsrf } from "./api";
import type { SessionResponse, User } from "./types";

interface AuthState {
  user: User | null;
  loading: boolean;
  needsSetup: boolean;
  refresh: () => Promise<void>;
}

const AuthContext = createContext<AuthState | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const setup = useQuery({
    queryKey: ["setup-status"],
    queryFn: () => api<{ needs_setup: boolean }>("/api/v1/setup/status"),
  });
  const session = useQuery({
    queryKey: ["session"],
    queryFn: async () => {
      try {
        const body = await api<SessionResponse>("/api/v1/me");
        if (body.csrf_token) {
          setCsrf(body.csrf_token);
        }
        return body.user;
      } catch (error) {
        if (error instanceof ApiError && error.status === 401) {
          setCsrf(null);
          return null;
        }
        throw error;
      }
    },
  });

  const value: AuthState = {
    user: session.data ?? null,
    loading: setup.isLoading || session.isLoading,
    needsSetup: setup.data?.needs_setup ?? false,
    refresh: async () => {
      await queryClient.invalidateQueries({ queryKey: ["session"] });
      await queryClient.invalidateQueries({ queryKey: ["setup-status"] });
    },
  };

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth() {
  const value = useContext(AuthContext);
  if (!value) {
    throw new Error("AuthProvider missing");
  }
  return value;
}

export function rememberSession(body: SessionResponse) {
  if (body.csrf_token) {
    setCsrf(body.csrf_token);
  }
}
