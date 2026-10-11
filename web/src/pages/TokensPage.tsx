import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Button, Card, Field, TextInput } from "../components/ui";
import type { CreatedToken, TokenView } from "../types";

export function TokensPage() {
  const queryClient = useQueryClient();
  const tokens = useQuery({
    queryKey: ["tokens"],
    queryFn: () => api<TokenView[]>("/api/v1/tokens"),
  });
  const [name, setName] = useState("");
  const [write, setWrite] = useState(false);
  const [revealed, setRevealed] = useState<string | null>(null);

  const create = useMutation({
    mutationFn: () =>
      api<CreatedToken>("/api/v1/tokens", {
        method: "POST",
        body: JSON.stringify({
          name,
          scopes: write ? ["read", "write"] : ["read"],
        }),
      }),
    onSuccess: async (created) => {
      setName("");
      setRevealed(created.token);
      toast.success("Token created. Copy it now; it is not shown again.");
      await queryClient.invalidateQueries({ queryKey: ["tokens"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not create token"),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/tokens/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["tokens"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : "Could not revoke token"),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">API tokens</h1>
      <Card title="New token">
        <form className="grid gap-3 md:grid-cols-[1fr_auto_auto]" onSubmit={onSubmit}>
          <Field label="Name">
            <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
          </Field>
          <label className="flex items-end gap-2 pb-2 text-sm">
            <input
              type="checkbox"
              checked={write}
              onChange={(event) => setWrite(event.target.checked)}
            />
            write scope
          </label>
          <div className="flex items-end">
            <Button type="submit" disabled={create.isPending}>
              Create
            </Button>
          </div>
        </form>
        {revealed ? (
          <p className="mt-4 break-all rounded-md bg-paper p-3 font-mono text-sm">{revealed}</p>
        ) : null}
      </Card>
      <Card title="Active tokens">
        <ul className="divide-y divide-line">
          {(tokens.data ?? []).map((token) => (
            <li key={token.id} className="flex items-center justify-between gap-3 py-3">
              <div>
                <p className="font-medium">{token.name}</p>
                <p className="text-sm text-muted">{token.scopes.join(", ")}</p>
              </div>
              <Button quiet onClick={() => remove.mutate(token.id)}>
                Revoke
              </Button>
            </li>
          ))}
        </ul>
      </Card>
    </>
  );
}
