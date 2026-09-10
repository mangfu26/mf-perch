<script setup lang="ts">
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useI18n } from "vue-i18n";
import { Server, KeyRound, TerminalSquare, Settings, Sun, Moon } from "lucide-vue-next";
import { useThemeStore } from "@/stores/theme";
import { useMcpStore } from "@/stores/mcp";
import { cn } from "@/lib/utils";

const { t } = useI18n();
const route = useRoute();
const router = useRouter();
const theme = useThemeStore();
const mcp = useMcpStore();

/** 一级导航仅 4 项，关于在设置内（D26）。 */
const navItems = [
  { name: "hosts", to: "/hosts", icon: Server, labelKey: "nav.hosts" },
  { name: "credentials", to: "/credentials", icon: KeyRound, labelKey: "nav.credentials" },
  { name: "terminals", to: "/terminals", icon: TerminalSquare, labelKey: "nav.terminals" },
  { name: "settings", to: "/settings", icon: Settings, labelKey: "nav.settings" },
];

function isActive(name: string) {
  // 终端详情页也应高亮"终端与审计"。
  if (name === "terminals") {
    return route.name === "terminals" || route.name === "terminal-detail";
  }
  return route.name === name;
}

const mcpStatusText = computed(() =>
  mcp.running ? t("mcp.running") : t("mcp.stopped"),
);
</script>

<template>
  <aside
    class="flex w-[220px] shrink-0 flex-col gap-6 border-r border-border-base bg-bg-elevated px-3.5 py-5 dark:backdrop-blur-xl"
  >
    <!-- 品牌 -->
    <div class="flex items-center gap-2.5 px-1.5">
      <div
        class="grid h-[29px] w-[29px] place-items-center rounded-lg bg-[linear-gradient(135deg,var(--accent),#6247e8)] text-[13px] font-extrabold text-white [box-shadow:var(--glow)]"
      >
        M
      </div>
      <span class="text-[15px] font-semibold tracking-tight">{{ t("app.name") }}</span>
    </div>

    <!-- 导航 -->
    <nav class="flex flex-col gap-0.5">
      <button
        v-for="item in navItems"
        :key="item.name"
        type="button"
        :class="
          cn(
            'flex items-center gap-2.5 rounded-[9px] px-2.5 py-2 text-left text-[13.5px] text-text-muted transition-colors',
            isActive(item.name)
              ? 'bg-accent-soft font-semibold text-text-base dark:text-white'
              : 'hover:bg-surface-hover',
          )
        "
        @click="router.push(item.to)"
      >
        <component :is="item.icon" class="h-[15px] w-[15px]" />
        {{ t(item.labelKey) }}
      </button>
    </nav>

    <div class="mt-auto flex flex-col gap-3">
      <!-- 主题切换（D24） -->
      <div class="flex gap-0.5 rounded-[10px] border border-border-base bg-surface p-[3px]">
        <button
          type="button"
          class="flex flex-1 items-center justify-center gap-1.5 rounded-[7px] px-2.5 py-1.5 text-[11.5px] text-text-muted transition-colors"
          :class="theme.resolved === 'dark' && 'bg-accent-soft font-semibold text-text-base dark:text-white'"
          @click="theme.setMode('dark')"
        >
          <Moon class="h-3.5 w-3.5" />
          {{ t("settings.themeDark") }}
        </button>
        <button
          type="button"
          class="flex flex-1 items-center justify-center gap-1.5 rounded-[7px] px-2.5 py-1.5 text-[11.5px] text-text-muted transition-colors"
          :class="theme.resolved === 'light' && 'bg-surface-hover font-semibold text-text-base'"
          @click="theme.setMode('light')"
        >
          <Sun class="h-3.5 w-3.5" />
          {{ t("settings.themeLight") }}
        </button>
      </div>

      <!-- MCP 状态卡 -->
      <button
        type="button"
        class="rounded-[11px] border border-border-base bg-surface px-3 py-2.5 text-left text-[12px] transition-colors hover:bg-surface-hover"
        @click="router.push('/settings')"
      >
        <div class="flex items-center justify-between">
          <span class="text-text-muted">{{ t("mcp.title") }}</span>
          <span
            class="h-[7px] w-[7px] rounded-full"
            :class="mcp.running ? 'bg-success' : 'bg-text-muted'"
            :style="mcp.running ? 'box-shadow:0 0 8px var(--success)' : undefined"
          />
        </div>
        <div class="mt-1.5 font-mono text-[11.5px] text-text-muted">
          <template v-if="mcp.running && mcp.endpoint">{{ mcp.endpoint }}</template>
          <template v-else>{{ mcpStatusText }}</template>
        </div>
      </button>
    </div>
  </aside>
</template>
