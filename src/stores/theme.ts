import { defineStore } from "pinia";
import { ref, computed, watch } from "vue";

/**
 * 主题状态（D24）。
 *
 * 三态：跟随系统 / 强制暗色 / 强制亮色，默认跟随系统。
 * 通过 `data-theme` 属性驱动 CSS 变量令牌，切换时布局不跳动。
 */
export type ThemeMode = "system" | "dark" | "light";
export type ResolvedTheme = "dark" | "light";

const STORAGE_KEY = "mf-perch.theme";

function readStoredMode(): ThemeMode {
  const v = localStorage.getItem(STORAGE_KEY);
  return v === "dark" || v === "light" || v === "system" ? v : "system";
}

const mediaQuery =
  typeof window !== "undefined"
    ? window.matchMedia("(prefers-color-scheme: dark)")
    : null;

export const useThemeStore = defineStore("theme", () => {
  const mode = ref<ThemeMode>(readStoredMode());
  const systemPrefersDark = ref(mediaQuery?.matches ?? true);

  if (mediaQuery) {
    mediaQuery.addEventListener("change", (e) => {
      systemPrefersDark.value = e.matches;
    });
  }

  /** 实际生效的主题。 */
  const resolved = computed<ResolvedTheme>(() => {
    if (mode.value === "system") {
      return systemPrefersDark.value ? "dark" : "light";
    }
    return mode.value;
  });

  function apply() {
    document.documentElement.setAttribute("data-theme", resolved.value);
  }

  function setMode(next: ThemeMode) {
    mode.value = next;
    localStorage.setItem(STORAGE_KEY, next);
  }

  /** 在暗色与亮色之间切换（把"跟随系统"解析为对立主题）。 */
  function toggle() {
    setMode(resolved.value === "dark" ? "light" : "dark");
  }

  watch(resolved, apply, { immediate: true });

  return { mode, resolved, systemPrefersDark, setMode, toggle, apply };
});
