import { Link } from "react-router-dom";
import { isInsecurePanel } from "../insecure";
import { useT } from "../i18n";
import { Icon } from "./icons";

/** A warning that stays on screen while the panel is reached over plain HTTP. */
export function InsecureBanner({ link = true }: { link?: boolean }) {
  const t = useT();
  if (!isInsecurePanel()) {
    return null;
  }
  return (
    <div
      role="alert"
      className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-amber-line bg-amber-soft px-4 py-2.5 text-sm text-amber sm:px-6 lg:px-8"
    >
      <Icon name="alert" size={16} className="shrink-0" />
      <p className="min-w-0 flex-1">
        <strong className="font-semibold">{t("This panel is not using HTTPS.")}</strong>{" "}
        {t("Your password, session and every secret you type here can be read by anyone on the network path.")}
      </p>
      {link ? (
        <Link to="/certificates" className="shrink-0 font-medium underline underline-offset-2">
          {t("Give the panel a domain with a certificate")}
        </Link>
      ) : null}
    </div>
  );
}
