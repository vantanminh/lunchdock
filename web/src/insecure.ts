// Detects a panel served over plain HTTP, where session cookies and every secret typed
// into the panel cross the network in clear text.

import { useT } from "./i18n";

const ACK_KEY = "rudder_http_secrets_ack";

function loopback(host: string): boolean {
  const name = host.replace(/^\[|\]$/g, "").toLowerCase();
  return (
    name === "localhost" ||
    name.endsWith(".localhost") ||
    name === "::1" ||
    /^127(\.\d{1,3}){3}$/.test(name)
  );
}

/** True when the page is not HTTPS and the host is not this computer. */
export function isInsecurePanel(location: Pick<Location, "protocol" | "hostname"> = window.location) {
  return location.protocol === "http:" && !loopback(location.hostname);
}

function acknowledged(): boolean {
  try {
    return sessionStorage.getItem(ACK_KEY) === "1";
  } catch {
    return false;
  }
}

/**
 * Ask before a secret is sent over plain HTTP. Returns true when the page is secure, when the
 * person already agreed in this browser tab, or when they agree now.
 */
function confirmSendSecret(message: string): boolean {
  if (!isInsecurePanel() || acknowledged()) {
    return true;
  }
  if (!window.confirm(message)) {
    return false;
  }
  try {
    sessionStorage.setItem(ACK_KEY, "1");
  } catch {
    // Without storage the question is simply asked again next time.
  }
  return true;
}

/** A function to call before sending a secret; false means the person declined. */
export function useConfirmSecret(): () => boolean {
  const t = useT();
  return () =>
    confirmSendSecret(
      t(
        "This panel is not using HTTPS, so this secret would travel unencrypted and could be read on the network. Save it anyway?",
      ),
    );
}
