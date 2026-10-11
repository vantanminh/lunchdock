import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Button, Card, Field, TextArea, TextInput } from "../components/ui";
import type { Schemas } from "../types";

type Certificate = Schemas["CertificateView"];

type PanelSettings = Schemas["PanelView"];

export function CertificatesPage() {
  const queryClient = useQueryClient();
  const certificates = useQuery({
    queryKey: ["certificates"],
    queryFn: () => api<Certificate[]>("/api/v1/certificates"),
  });
  const panel = useQuery({
    queryKey: ["panel"],
    queryFn: () => api<PanelSettings>("/api/v1/panel"),
  });
  const [name, setName] = useState("");
  const [certPem, setCertPem] = useState("");
  const [keyPem, setKeyPem] = useState("");
  const [chainPem, setChainPem] = useState("");
  const [domain, setDomain] = useState("");
  const [sslMode, setSslMode] = useState("letsencrypt");
  const [email, setEmail] = useState("");

  const upload = useMutation({
    mutationFn: () =>
      api("/api/v1/certificates", {
        method: "POST",
        body: JSON.stringify({
          name,
          cert_pem: certPem,
          key_pem: keyPem,
          chain_pem: chainPem || null,
        }),
      }),
    onSuccess: async () => {
      setCertPem("");
      setKeyPem("");
      setChainPem("");
      toast.success("Certificate stored");
      await queryClient.invalidateQueries({ queryKey: ["certificates"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Upload failed"),
  });

  const savePanel = useMutation({
    mutationFn: () =>
      api("/api/v1/panel", {
        method: "PATCH",
        body: JSON.stringify({
          panel_domain: domain,
          panel_ssl_mode: sslMode,
          acme_email: email || null,
          clear_domain: domain.trim() === "",
        }),
      }),
    onSuccess: async () => {
      toast.success("Panel domain saved");
      await queryClient.invalidateQueries({ queryKey: ["panel"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : "Could not save"),
  });

  function onUpload(event: FormEvent) {
    event.preventDefault();
    upload.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">Certificates</h1>
      <Card title="Upload">
        <form className="space-y-3" onSubmit={onUpload}>
          <Field label="Name">
            <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
          </Field>
          <Field label="Certificate PEM">
            <TextArea
              required
              value={certPem}
              onChange={(event) => setCertPem(event.target.value)}
            />
          </Field>
          <Field label="Private key PEM">
            <TextArea required value={keyPem} onChange={(event) => setKeyPem(event.target.value)} />
          </Field>
          <Field label="Chain PEM">
            <TextArea value={chainPem} onChange={(event) => setChainPem(event.target.value)} />
          </Field>
          <Button type="submit">Store certificate</Button>
        </form>
      </Card>
      <Card title="Stored">
        <ul className="space-y-3 text-sm">
          {(certificates.data ?? []).map((certificate) => (
            <li key={certificate.id} className="rounded-md border border-line p-3">
              <p className="font-medium">{certificate.name}</p>
              <p className="text-muted">CN {certificate.common_name ?? "—"}</p>
              <p>SANs {certificate.sans.join(", ") || "—"}</p>
              <p className="text-muted">Issuer {certificate.issuer ?? "—"}</p>
              <p className="text-muted">
                Expires{" "}
                {certificate.not_after ? new Date(certificate.not_after).toLocaleString() : "—"}
              </p>
            </li>
          ))}
          {certificates.data?.length === 0 ? (
            <p className="text-muted">No certificates yet.</p>
          ) : null}
        </ul>
      </Card>
      <Card title="Panel domain">
        <form
          className="grid gap-3 md:grid-cols-2"
          onSubmit={(event) => {
            event.preventDefault();
            savePanel.mutate();
          }}
        >
          <Field label="Domain">
            <TextInput
              value={domain || panel.data?.panel_domain || ""}
              onChange={(event) => setDomain(event.target.value)}
              placeholder="panel.example.com"
            />
          </Field>
          <Field label="SSL">
            <select
              className="h-10 w-full rounded-md border border-line bg-paper px-3 text-sm"
              value={sslMode}
              onChange={(event) => setSslMode(event.target.value)}
            >
              <option value="letsencrypt">Let's Encrypt</option>
              <option value="custom">Uploaded certificate</option>
              <option value="none">None</option>
            </select>
          </Field>
          <Field label="ACME email">
            <TextInput value={email} onChange={(event) => setEmail(event.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit">Save</Button>
          </div>
        </form>
        <p className="mt-3 text-sm text-muted">
          Matching certificate: {panel.data?.matched_certificate_name ?? "none yet"}
        </p>
      </Card>
    </>
  );
}
