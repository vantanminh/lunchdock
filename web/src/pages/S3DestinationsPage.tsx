import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { Button, Card, Empty, Field, PickField, QuickPick, TextInput } from "../components/ui";
import { errorMessage, formatTime } from "../format";
import { useT } from "../i18n";
import { S3_PROVIDERS, S3_REGIONS } from "../presets";
import type { Schemas } from "../types";

type Destination = Schemas["DestinationView"];

const blank = {
  name: "",
  endpoint: "",
  region: "us-east-1",
  bucket: "",
  access_key_id: "",
  secret_access_key: "",
  path_style: true,
};

const slug = (label: string) => label.toLowerCase().replace(/[^a-z0-9]+/g, "-");

export function S3DestinationsPage() {
  const t = useT();
  const queryClient = useQueryClient();
  const destinations = useQuery({
    queryKey: ["s3-destinations"],
    queryFn: () => api<Destination[]>("/api/v1/s3-destinations"),
  });
  const [form, setForm] = useState(blank);
  const [editing, setEditing] = useState<string | null>(null);

  const save = useMutation({
    mutationFn: () => {
      const body = {
        ...form,
        endpoint: form.endpoint || null,
        access_key_id: form.access_key_id || undefined,
        secret_access_key: form.secret_access_key || undefined,
      };
      return editing
        ? api<Destination>(`/api/v1/s3-destinations/${editing}`, {
            method: "PUT",
            body: JSON.stringify(body),
          })
        : api<Destination>("/api/v1/s3-destinations", {
            method: "POST",
            body: JSON.stringify(body),
          });
    },
    onSuccess: async () => {
      toast.success(editing ? "Destination updated" : "Destination added");
      setForm(blank);
      setEditing(null);
      await queryClient.invalidateQueries({ queryKey: ["s3-destinations"] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not save destination")),
  });

  const test = useMutation({
    mutationFn: (id: string) =>
      api<Schemas["TestResult"]>(`/api/v1/s3-destinations/${id}/test`, { method: "POST" }),
    onSuccess: (result) =>
      result.ok ? toast.success(result.message) : toast.error(result.message),
    onError: (error) => toast.error(errorMessage(error, "Test failed")),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/v1/s3-destinations/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      toast.success("Destination removed");
      await queryClient.invalidateQueries({ queryKey: ["s3-destinations"] });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not remove destination")),
  });

  function edit(destination: Destination) {
    setEditing(destination.id);
    setForm({
      name: destination.name,
      endpoint: destination.endpoint ?? "",
      region: destination.region,
      bucket: destination.bucket,
      access_key_id: "",
      secret_access_key: "",
      path_style: destination.path_style,
    });
  }

  const pickProvider = (label: string) => {
    const provider = S3_PROVIDERS.find((item) => item.label === label);
    if (!provider) return;
    setForm({
      ...form,
      name: form.name || `${slug(provider.label)}-backups`,
      endpoint: provider.endpoint,
      region: provider.region,
      path_style: provider.path_style,
    });
  };
  const set = (key: keyof typeof blank) => (event: { target: { value: string } }) =>
    setForm({ ...form, [key]: event.target.value });

  return (
    <>
      <h1 className="text-2xl font-semibold">S3 destinations</h1>
      <p className="text-sm text-muted">
        Any S3-compatible bucket: AWS S3, Cloudflare R2, Backblaze B2, Wasabi, RustFS, Garage, or
        MinIO. Keys are encrypted at rest and never shown again.
      </p>
      <Card title={editing ? "Edit destination" : "New destination"}>
        <form
          className="grid gap-3 md:grid-cols-2"
          onSubmit={(event) => {
            event.preventDefault();
            save.mutate();
          }}
        >
          <div className="md:col-span-2">
            <PickField
              label="Provider"
              hint={t(
                "Fills in the endpoint, region, and addressing style. You can still edit them.",
              )}
              options={S3_PROVIDERS.map((provider) => ({
                label: provider.label,
                value: provider.label,
              }))}
              value={
                S3_PROVIDERS.find(
                  (item) => item.endpoint === form.endpoint && item.region === form.region,
                )?.label ?? ""
              }
              onPick={pickProvider}
            />
          </div>
          <Field label="Name">
            <TextInput required value={form.name} onChange={set("name")} />
          </Field>
          <Field
            label="Endpoint (empty for AWS)"
            hint={
              form.endpoint.includes("<")
                ? t("Replace the <…> part with your own value before saving.")
                : undefined
            }
          >
            <TextInput
              value={form.endpoint}
              placeholder="https://<account>.r2.cloudflarestorage.com"
              pattern="[^<>]*"
              title={t("Replace the <…> part with your own value.")}
              onChange={set("endpoint")}
            />
          </Field>
          <Field
            label="Region"
            presets={
              <QuickPick
                label={t("Region presets")}
                options={S3_REGIONS}
                value={form.region}
                onPick={(region) => setForm({ ...form, region })}
              />
            }
          >
            <TextInput required value={form.region} onChange={set("region")} />
          </Field>
          <Field label="Bucket">
            <TextInput required value={form.bucket} onChange={set("bucket")} />
          </Field>
          <Field label={editing ? "Access key ID (leave empty to keep)" : "Access key ID"}>
            <TextInput
              required={!editing}
              autoComplete="off"
              value={form.access_key_id}
              onChange={set("access_key_id")}
            />
          </Field>
          <Field label={editing ? "Secret access key (leave empty to keep)" : "Secret access key"}>
            <TextInput
              required={!editing}
              type="password"
              autoComplete="new-password"
              value={form.secret_access_key}
              onChange={set("secret_access_key")}
            />
          </Field>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={form.path_style}
              onChange={(event) => setForm({ ...form, path_style: event.target.checked })}
            />
            Path-style addressing (needed by most self-hosted servers)
          </label>
          <div className="flex items-end gap-2">
            <Button type="submit" disabled={save.isPending}>
              {editing ? "Save" : "Add destination"}
            </Button>
            {editing ? (
              <Button
                quiet
                type="button"
                onClick={() => {
                  setEditing(null);
                  setForm(blank);
                }}
              >
                Cancel
              </Button>
            ) : null}
          </div>
        </form>
      </Card>
      <Card title="Destinations">
        <ul className="divide-y divide-line text-sm">
          {(destinations.data ?? []).map((destination) => (
            <li key={destination.id} className="flex flex-wrap items-center gap-3 py-3">
              <div className="min-w-0 flex-1">
                <p className="font-medium">{destination.name}</p>
                <p className="truncate text-muted">
                  {destination.endpoint ?? "AWS S3"} · {destination.bucket} · {destination.region} ·
                  key {destination.access_key_hint} · added {formatTime(destination.created_at)}
                </p>
              </div>
              <Button quiet onClick={() => test.mutate(destination.id)} disabled={test.isPending}>
                Test
              </Button>
              <Button quiet onClick={() => edit(destination)}>
                Edit
              </Button>
              <Button quiet onClick={() => remove.mutate(destination.id)}>
                Remove
              </Button>
            </li>
          ))}
        </ul>
        {destinations.data?.length === 0 ? <Empty>No destinations yet.</Empty> : null}
      </Card>
    </>
  );
}
