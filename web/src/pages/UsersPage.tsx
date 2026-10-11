import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { useAuth } from "../auth";
import { Button, Card, Field, PickField, TextInput } from "../components/ui";
import { ROLES } from "../presets";
import type { Role, User } from "../types";
import { useT } from "../i18n";

export function UsersPage() {
  const t = useT();
  const { user } = useAuth();
  const queryClient = useQueryClient();
  const users = useQuery({
    queryKey: ["users"],
    queryFn: () => api<User[]>("/api/v1/users"),
    enabled: user?.role === "admin",
  });
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState<Role>("viewer");

  const create = useMutation({
    mutationFn: () =>
      api<User>("/api/v1/users", {
        method: "POST",
        body: JSON.stringify({ email, password, role }),
      }),
    onSuccess: async () => {
      setEmail("");
      setPassword("");
      toast.success(t("User created"));
      await queryClient.invalidateQueries({ queryKey: ["users"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not create user")),
  });

  const updateRole = useMutation({
    mutationFn: (input: { id: string; role: Role }) =>
      api(`/api/v1/users/${input.id}`, {
        method: "PATCH",
        body: JSON.stringify({ role: input.role }),
      }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["users"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not update role")),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/users/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success(t("User deleted"));
      await queryClient.invalidateQueries({ queryKey: ["users"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not delete user")),
  });

  if (user?.role !== "admin") {
    return <p className="text-sm text-muted">{t("Only admins can manage users.")}</p>;
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">{t("Users")}</h1>
      <Card title={t("Add user")}>
        <form className="grid gap-3 md:grid-cols-4" onSubmit={onSubmit}>
          <Field label={t("Email")}>
            <TextInput
              type="email"
              required
              value={email}
              onChange={(event) => setEmail(event.target.value)}
            />
          </Field>
          <Field label={t("Password")}>
            <TextInput
              type="password"
              required
              minLength={12}
              value={password}
              onChange={(event) => setPassword(event.target.value)}
            />
          </Field>
          <PickField
            label={t("Role")}
            options={ROLES}
            value={role}
            onPick={(next) => setRole(next as Role)}
          />
          <div className="flex items-end">
            <Button type="submit" disabled={create.isPending}>
              {t("Create")}
            </Button>
          </div>
        </form>
      </Card>
      <Card title={t("Directory")}>
        <ul className="divide-y divide-line">
          {(users.data ?? []).map((row) => (
            <li key={row.id} className="flex flex-wrap items-center justify-between gap-3 py-3">
              <div>
                <p className="font-medium">{row.display_name}</p>
                <p className="text-sm text-muted">{row.email}</p>
              </div>
              <div className="flex items-center gap-2">
                <select
                  className="h-10 rounded-md border border-line bg-paper px-2 text-sm"
                  value={row.role}
                  onChange={(event) =>
                    updateRole.mutate({ id: row.id, role: event.target.value as Role })
                  }
                >
                  <option value="viewer">{t("viewer")}</option>
                  <option value="admin">{t("admin")}</option>
                </select>
                <Button quiet disabled={row.id === user.id} onClick={() => remove.mutate(row.id)}>
                  {t("Delete")}
                </Button>
              </div>
            </li>
          ))}
        </ul>
      </Card>
    </>
  );
}
