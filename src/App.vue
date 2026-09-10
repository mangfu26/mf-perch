<script setup lang="ts">
/**
 * 应用外壳：侧边栏 + 主内容区 + 全局提示。
 *
 * 全局提示承载后端错误（如密钥未解锁、MCP 启动失败），
 * 避免错误只落在某个组件里被忽略（P1：明确报错）。
 */
import { onMounted } from "vue";
import { useI18n } from "vue-i18n";
import { AlertCircle, CheckCircle2, X } from "lucide-vue-next";
import AppSidebar from "@/components/layout/AppSidebar.vue";
import { useAppStore } from "@/stores/app";
import { useMcpStore } from "@/stores/mcp";

const app = useAppStore();
const mcp = useMcpStore();
const { t } = useI18n();

onMounted(async () => {
  // 启动时同步密钥状态与 MCP 状态，使界面反映后端真实情况。
  await app.refreshKeyStatus();
  await mcp.refresh();
  await mcp.loadClientConfig();
});
</script>

<template>
  <div class="relative z-1 flex h-full w-full">
    <AppSidebar />
    <main class="flex min-w-0 flex-1 flex-col overflow-hidden">
      <!-- 密钥未就绪时的全局提示：影响凭据相关功能 -->
      <div
        v-if="app.needsSetup || app.needsUnlock"
        class="flex shrink-0 items-center justify-between gap-3 border-b border-warning/30 bg-warning-soft px-5 py-2.5"
      >
        <span class="text-[12.5px] text-warning">
          {{
            app.needsSetup
              ? "尚未设置凭据保护方式，无法保存 SSH 密码与私钥。"
              : "凭据尚未解锁，请前往设置输入主密码。"
          }}
        </span>
      </div>

      <div class="min-h-0 flex-1">
        <RouterView />
      </div>
    </main>

    <!-- 全局提示 -->
    <Transition
      enter-active-class="transition duration-200"
      enter-from-class="opacity-0 translate-y-2"
      leave-active-class="transition duration-150"
      leave-to-class="opacity-0 translate-y-2"
    >
      <div
        v-if="app.lastError"
        class="fixed bottom-5 right-5 z-60 flex max-w-md items-start gap-2.5 rounded-xl border border-danger/30 bg-bg-elevated px-4 py-3 shadow-2xl"
      >
        <AlertCircle class="mt-0.5 h-4 w-4 shrink-0 text-danger" />
        <p class="text-[12.5px] leading-relaxed text-text-base">
          {{ app.lastError }}
        </p>
        <button
          type="button"
          class="ml-1 rounded p-0.5 text-text-muted hover:text-text-base"
          @click="app.clearError()"
        >
          <X class="h-3.5 w-3.5" />
        </button>
      </div>
    </Transition>

    <Transition
      enter-active-class="transition duration-200"
      enter-from-class="opacity-0 translate-y-2"
      leave-active-class="transition duration-150"
      leave-to-class="opacity-0 translate-y-2"
    >
      <div
        v-if="app.lastNotice"
        class="fixed bottom-5 right-5 z-60 flex items-center gap-2.5 rounded-xl border border-border-base bg-bg-elevated px-4 py-3 shadow-2xl"
      >
        <CheckCircle2 class="h-4 w-4 shrink-0 text-success" />
        <p class="text-[12.5px] text-text-base">{{ app.lastNotice }}</p>
      </div>
    </Transition>
  </div>
  <span class="sr-only">{{ t("app.tagline") }}</span>
</template>
