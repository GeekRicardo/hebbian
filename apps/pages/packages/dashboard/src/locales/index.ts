import en from "./en";
import zh from "./zh";
import zhTW from "./zh-TW";
import ja from "./ja";
import ko from "./ko";

export type LocaleKey = "en" | "zh" | "zh-TW" | "ja" | "ko";
export type Locale = typeof en;

export const locales: Record<LocaleKey, Locale> = {
  en,
  zh,
  "zh-TW": zhTW,
  ja,
  ko,
};

export function getLocale(key: LocaleKey): Locale {
  return locales[key] ?? locales.en;
}

export function resolveLocaleKey(lang: string | undefined): LocaleKey {
  if (!lang) return "en";
  const normalized = lang.toLowerCase().replace(/[_\s]/g, "-");
  if (normalized === "zh" || normalized === "chinese" || normalized === "zh-cn") return "zh";
  if (normalized === "zh-tw" || normalized === "traditional-chinese") return "zh-TW";
  if (normalized === "ja" || normalized === "japanese") return "ja";
  if (normalized === "ko" || normalized === "korean") return "ko";
  return "en";
}

export { en, zh, zhTW as "zh-TW", ja, ko };