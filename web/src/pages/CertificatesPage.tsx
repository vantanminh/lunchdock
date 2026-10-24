import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";
import { api, ApiError } from "../api";
import { Alert, Button, Card, Field, PickField, TextArea, TextInput } from "../components/ui";
import { isInsecurePanel } from "../insecure";
import { SSL_MODES } from "../presets";
import type { Schemas } from "../types";
import { formatTime } from "../format";
import { useT } from "../i18n";

type Certificate = Schemas["CertificateView"];

type PanelSettings = Schemas["PanelView"];

type SslCheck = Schemas["PanelSslCheck"];

export function CertificatesPage() {
  const t = useT();
  const queryClient = useQueryClient();
  const certificates = useQuery({
    queryKey: ["certificates"],
    queryFn: () => api<Certificate[]>("/api/v1/certificates"),
  });
  const panel = useQuery({
    queryKey: ["panel"],
    queryFn: () => api<PanelSettings>("/api/v1/panel"),
  });
  const sslCheck = useQuery({
    queryKey: ["panel-ssl-check"],
    queryFn: () => api<SslCheck>("/api/v1/panel/ssl-check"),
    enabled: false,
    retry: false,
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
      toast.success(t("Certificate stored"));
      await queryClient.invalidateQueries({ queryKey: ["certificates"] });
    },
    onError: (error) => toast.error(error instanceof ApiError ? error.message : t("Upload failed")),
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
      toast.success(t("Panel domain saved"));
      await queryClient.invalidateQueries({ queryKey: ["panel"] });
      queryClient.removeQueries({ queryKey: ["panel-ssl-check"] });
    },
    onError: (error) =>
      toast.error(error instanceof ApiError ? error.message : t("Could not save")),
  });

  function onUpload(event: FormEvent) {
    event.preventDefault();
    upload.mutate();
  }

  return (
    <>
      <h1 className="text-2xl font-semibold">{t("Certificates")}</h1>
      <Card title={t("Upload")}>
        <form className="space-y-3" onSubmit={onUpload}>
          <Field label={t("Name")}>
            <TextInput required value={name} onChange={(event) => setName(event.target.value)} />
          </Field>
          <Field label={t("Certificate PEM")}>
            <TextArea
              required
              value={certPem}
              onChange={(event) => setCertPem(event.target.value)}
            />
          </Field>
          <Field label={t("Private key PEM")}>
            <TextArea required value={keyPem} onChange={(event) => setKeyPem(event.target.value)} />
          </Field>
          <Field label={t("Chain PEM")}>
            <TextArea value={chainPem} onChange={(event) => setChainPem(event.target.value)} />
          </Field>
          <Button type="submit">{t("Store certificate")}</Button>
        </form>
      </Card>
      <Card title={t("Stored")}>
        <ul className="space-y-3 text-sm">
          {(certificates.data ?? []).map((certificate) => (
            <li key={certificate.id} className="rounded-md border border-line p-3">
              <p className="font-medium">{certificate.name}</p>
              <p className="text-muted">CN {certificate.common_name ?? "—"}</p>
              <p>SANs {certificate.sans.join(", ") || "—"}</p>
              <p className="text-muted">
                {t("Issuer")} {certificate.issuer ?? "—"}
              </p>
              <p className="text-muted">
                {t("Expires")} {formatTime(certificate.not_after)}
              </p>
            </li>
          ))}
          {certificates.data?.length === 0 ? (
            <p className="text-muted">{t("No certificates yet.")}</p>
          ) : null}
        </ul>
      </Card>
      <Card title={t("Panel domain")}>
        {isInsecurePanel() ? (
          <div className="mb-4">
            <Alert tone="warning" title={t("You are reaching the panel over plain HTTP")}>
              {t(
                "Point a domain at this server, enter it below with an SSL mode, and open the panel through https:// so passwords and secrets are encrypted in transit.",
              )}
            </Alert>
          </div>
        ) : null}
        <form
          className="grid gap-3 md:grid-cols-2"
          onSubmit={(event) => {
            event.preventDefault();
            savePanel.mutate();
          }}
        >
          <Field label={t("Domain")}>
            <TextInput
              value={domain || panel.data?.panel_domain || ""}
              onChange={(event) => setDomain(event.target.value)}
              placeholder="panel.example.com"
            />
          </Field>
          <PickField label={t("SSL")} options={SSL_MODES} value={sslMode} onPick={setSslMode} />
          <Field label={t("ACME email")}>
            <TextInput value={email} onChange={(event) => setEmail(event.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit">{t("Save")}</Button>
          </div>
        </form>
        <p className="mt-3 text-sm text-muted">
          {t("Matching certificate: {name}", {
            name: panel.data?.matched_certificate_name ?? t("none yet"),
          })}
        </p>
        <div className="mt-4 space-y-3 border-t border-line pt-4 text-sm">
          <div className="flex flex-wrap items-center gap-3">
            <Button
              type="button"
              disabled={!panel.data?.panel_domain || sslCheck.isFetching}
              onClick={() => void sslCheck.refetch()}
            >
              {sslCheck.isFetching ? t("Checking…") : t("Check SSL")}
            </Button>
            <span className="text-muted">
              {t("Connects to the saved panel domain over HTTPS, as a browser or an AI agent would.")}
            </span>
          </div>
          {sslCheck.isError ? (
            <Alert tone="warning" title={t("Could not run the check")}>
              {sslCheck.error instanceof ApiError ? sslCheck.error.message : t("Try again")}
            </Alert>
          ) : null}
          {sslCheck.data ? <SslResult result={sslCheck.data} /> : null}
        </div>
      </Card>
    </>
  );
}

function SslResult({ result }: { result: SslCheck }) {
  const t = useT();
  const healthy =
    result.reachable &&
    result.trusted &&
    result.hostname_matches &&
    (result.days_left ?? 0) >= 0;
  const soon = healthy && (result.days_left ?? 0) < 14;
  return (
    <div className="space-y-2">
      <Alert
        tone={healthy && !soon ? "success" : "warning"}
        title={
          healthy
            ? soon
              ? t("The certificate is valid but expires soon")
              : t("The certificate is valid")
            : t("The certificate has a problem")
        }
      >
        {result.error ?? ""}
      </Alert>
      {result.reachable ? (
        <ul className="space-y-1">
          <li>
            {t("Trusted by browsers")}: {result.trusted ? t("Yes") : t("No")}
          </li>
          <li>
            {t("Covers {domain}", { domain: result.domain ?? "" })}:{" "}
            {result.hostname_matches ? t("Yes") : t("No")}
          </li>
          <li>CN {result.common_name ?? "—"}</li>
          <li>SANs {result.sans.join(", ") || "—"}</li>
          <li>
            {t("Issuer")} {result.issuer ?? "—"}
          </li>
          <li>
            {t("Expires")} {formatTime(result.not_after)}
            {result.days_left != null ? ` (${t("{days} days left", { days: result.days_left })})` : ""}
          </li>
        </ul>
      ) : null}
    </div>
  );
}
