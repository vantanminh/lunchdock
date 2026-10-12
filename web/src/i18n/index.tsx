import {
  createContext,
  Fragment,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { vi } from "./vi";

export type Language = "en" | "vi";

export const LANGUAGES: readonly { code: Language; label: string; locale: string }[] = [
  { code: "en", label: "English", locale: "en-US" },
  { code: "vi", label: "Tiếng Việt", locale: "vi-VN" },
];

const STORAGE_KEY = "rudder_lang";

/** English text is the lookup key; a language without an entry shows the English text. */
const dictionaries: Record<Language, Record<string, string>> = { en: {}, vi };

function isLanguage(value: unknown): value is Language {
  return LANGUAGES.some((language) => language.code === value);
}

function detectLanguage(): Language {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (isLanguage(stored)) {
      return stored;
    }
  } catch {
    // Storage can be blocked; fall through to the browser language.
  }
  return navigator.language?.toLowerCase().startsWith("vi") ? "vi" : "en";
}

let current: Language = detectLanguage();

/** The active language, for code that formats values outside React (see format.ts). */
export function currentLanguage(): Language {
  return current;
}

/** BCP 47 locale of the active language, for Intl and toLocale* calls. */
export function currentLocale(): string {
  return LANGUAGES.find((language) => language.code === current)?.locale ?? "en-US";
}

type Value = string | number;

export interface Translate {
  (key: string, values?: Record<string, Value>): string;
  /** With element values the result is a fragment list, so links and bold text can sit inline. */
  (key: string, values: Record<string, ReactNode>): ReactNode;
}

function fill(template: string, values?: Record<string, ReactNode>): ReactNode {
  if (!values) {
    return template;
  }
  const parts = template.split(/(\{\w+\})/g).map((part) => {
    const name = /^\{(\w+)\}$/.exec(part)?.[1];
    return name !== undefined && name in values ? values[name] : part;
  });
  if (parts.every((part) => typeof part === "string" || typeof part === "number")) {
    return parts.join("");
  }
  return parts.map((part, index) => <Fragment key={index}>{part}</Fragment>);
}

export function translate(
  language: Language,
  key: string,
  values?: Record<string, ReactNode>,
): ReactNode {
  return fill(dictionaries[language][key] ?? key, values);
}

/**
 * Marks a string for translation without translating it yet. Use it for text kept in
 * module-level constants, then pass the value through `t()` where it is rendered.
 */
export const msg = (text: string): string => text;

interface LanguageState {
  language: Language;
  setLanguage: (language: Language) => void;
  t: Translate;
}

const LanguageContext = createContext<LanguageState | null>(null);

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [language, setLanguageState] = useState<Language>(current);

  const value = useMemo<LanguageState>(
    () => ({
      language,
      setLanguage: (next) => {
        current = next;
        try {
          localStorage.setItem(STORAGE_KEY, next);
        } catch {
          // The choice still applies until the page is reloaded.
        }
        setLanguageState(next);
      },
      t: ((key: string, values?: Record<string, ReactNode>) =>
        translate(language, key, values)) as Translate,
    }),
    [language],
  );

  useEffect(() => {
    document.documentElement.lang = language;
  }, [language]);

  return <LanguageContext.Provider value={value}>{children}</LanguageContext.Provider>;
}

export function useLanguage(): LanguageState {
  const value = useContext(LanguageContext);
  if (!value) {
    throw new Error("LanguageProvider missing");
  }
  return value;
}

export function useT(): Translate {
  return useLanguage().t;
}
