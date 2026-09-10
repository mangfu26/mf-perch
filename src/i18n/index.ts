import { createI18n } from "vue-i18n";
import zhCN from "./locales/zh-CN";

/**
 * i18n 初始化（D18）。
 *
 * 首发仅提供 zh-CN 语言包；结构上已预留多语言能力，
 * 新增语言只需补充 `locales/<lang>.ts` 并注册到 `messages`。
 */
export const SUPPORTED_LOCALES = [
  { value: "zh-CN", label: "简体中文" },
] as const;

export type LocaleCode = (typeof SUPPORTED_LOCALES)[number]["value"];

export const DEFAULT_LOCALE: LocaleCode = "zh-CN";

export const i18n = createI18n({
  legacy: false,
  globalInjection: true,
  locale: DEFAULT_LOCALE,
  fallbackLocale: DEFAULT_LOCALE,
  messages: {
    "zh-CN": zhCN,
  },
});

export function setLocale(locale: LocaleCode) {
  i18n.global.locale.value = locale;
}

/** 供非组件环境（如 Pinia store）使用的翻译函数。 */
export function t(key: string, params?: Record<string, unknown>): string {
  return params
    ? i18n.global.t(key, params as Record<string, unknown>)
    : i18n.global.t(key);
}
