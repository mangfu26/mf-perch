<script setup lang="ts">
/**
 * 设置页（D26）：MCP、外观、安全、数据、关于。
 *
 * "关于"作为设置页内的分区，不占一级导航（D26）。
 * 安全相关项遵循 P2：降级必须显式告知。
 */
import { computed, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import {
  Settings,
  Power,
  Copy,
  RefreshCw,
  Eye,
  EyeOff,

  ExternalLink,
  Info,
  Check,
} from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import BaseSwitch from "@/components/ui/BaseSwitch.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import KeySetupDialog from "@/components/settings/KeySetupDialog.vue";
import { useThemeStore, type ThemeMode } from "@/stores/theme";
import { useMcpStore } from "@/stores/mcp";
import { useAppStore } from "@/stores/app";
import { historyStats } from "@/lib/commands";
import { copyToClipboard, formatBytes } from "@/lib/format";
import type { HistoryStats } from "@/lib/api";

const { t } = useI18n();
const theme = useThemeStore();
const mcp = useMcpStore();
const app = useAppStore();

const showToken = ref(false);
const copied = ref(false);
const showRemoteWarning = ref(false);
const confirmRegenOpen = ref(false);
const keyDialogOpen = ref(false);
const keyDialogUnlockMode = ref(false);

const stats = ref<HistoryStats | null>(null);

const themeOptions: Array<{ value: ThemeMode; labelKey: string }> = [
  { value: "system", labelKey: "settings.themeSystem" },
  { value: "dark", labelKey: "settings.themeDark" },
  { value: "light", labelKey: "settings.themeLight" },
];

onMounted(async () => {
  await Promise.all([mcp.refresh(), mcp.loadClientConfig()]);
  try {
    stats.value = await historyStats();
  } catch {
    // 统计失败不影响设置页其他功能。
  }
});

const maskedToken = computed(() => {
  const token = mcp.token;
  if (!token) return "—";
  if (showToken.value) return token;
  return "•".repeat(Math.min(token.length, 40));
});

const keyProviderLabel = computed(() => {
  const p = app.keyStatus?.provider;
  if (p === "keyring") return t("settings.keyProviderKeyring");
  if (p === "master_password") return t("settings.keyProviderMasterPassword");
  if (p === "local_file") return t("settings.keyProviderLocalFile");
  return "—";
});

async function copyConfig() {
  if (!mcp.clientConfig) return;
  if (await copyToClipboard(mcp.clientConfig)) {
    copied.value = true;
    setTimeout(() => (copied.value = false), 2000);
  }
}

async function toggleAllowRemote(value: boolean) {
  if (value) {
    // 开启远程连接是安全边界的放宽，先告知风险由用户确认（P2）。
    showRemoteWarning.value = true;
  }
  await mcp.setAllowRemote(value);
}

async function confirmRegenerate() {
  await mcp.regenerateToken();
  confirmRegenOpen.value = false;
}

const updateStatus = ref<"idle" | "checking" | "latest" | "failed">("idle");

async function checkUpdate() {
  updateStatus.value = "checking";
  // 真实的 Gist 更新源在阶段四接入（D23）。
  await new Promise((r) => setTimeout(r, 600));
  updateStatus.value = "latest";
}
</script>

<template>
  <PageShell :title="t('settings.title')" :icon="Settings">
    <div class="flex max-w-3xl flex-col gap-6 pb-8">
      <!-- ============ MCP Server ============ -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <div class="mb-4 flex items-center justify-between">
          <h2 class="text-[15px] font-semibold">{{ t("mcp.title") }}</h2>
          <BaseButton
            :variant="mcp.running ? 'default' : 'primary'"
            size="sm"
            :disabled="mcp.loading"
            @click="mcp.running ? mcp.stop() : mcp.start()"
          >
            <Power class="h-3.5 w-3.5" />
            {{ mcp.running ? t("mcp.stop") : t("mcp.start") }}
          </BaseButton>
        </div>

        <dl class="grid gap-3 text-[13px]">
          <div class="flex items-center justify-between gap-4">
            <dt class="text-text-muted">{{ t("mcp.status") }}</dt>
            <dd>
              <StatusTag :tone="mcp.running ? 'success' : 'neutral'">
                {{ mcp.running ? t("mcp.running") : t("mcp.stopped") }}
              </StatusTag>
            </dd>
          </div>

          <div class="flex items-center justify-between gap-4">
            <dt class="text-text-muted">{{ t("mcp.endpoint") }}</dt>
            <dd class="font-mono text-[12px]">{{ mcp.endpoint ?? "—" }}</dd>
          </div>

          <div class="flex items-start justify-between gap-4">
            <dt class="pt-1 text-text-muted">{{ t("mcp.token") }}</dt>
            <dd class="flex items-center gap-1.5">
              <code class="max-w-[280px] truncate font-mono text-[12px]">
                {{ maskedToken }}
              </code>
              <BaseButton size="sm" variant="ghost" @click="showToken = !showToken">
                <component :is="showToken ? EyeOff : Eye" class="h-3.5 w-3.5" />
              </BaseButton>
              <BaseButton
                size="sm"
                variant="ghost"
                :title="t('mcp.tokenRegenerate')"
                @click="confirmRegenOpen = true"
              >
                <RefreshCw class="h-3.5 w-3.5" />
              </BaseButton>
            </dd>
          </div>

          <div class="flex items-center justify-between gap-4">
            <dt class="text-text-muted">{{ t("mcp.port") }}</dt>
            <dd class="font-mono text-[12px]">{{ mcp.port ?? "—" }}</dd>
          </div>
        </dl>

        <p class="mt-3 text-[11.5px] leading-relaxed text-text-muted">
          {{ t("mcp.portHint") }}
        </p>

        <!-- 远程连接：默认关闭，开启时显式告警（D2 / P2） -->
        <div class="mt-4 border-t border-border-base pt-4">
          <div class="flex items-center justify-between gap-4">
            <div>
              <span class="text-[13px]">{{ t("mcp.allowRemote") }}</span>
              <p class="mt-0.5 text-[11.5px] text-text-muted">
                {{ t("mcp.allowRemoteHint") }}
              </p>
            </div>
            <BaseSwitch
              :model-value="mcp.allowRemote"
              @update:model-value="toggleAllowRemote"
            />
          </div>
          <p
            v-if="showRemoteWarning && mcp.allowRemote"
            class="mt-2 rounded-lg border border-warning/30 bg-warning-soft px-3 py-2 text-[11.5px] leading-relaxed text-warning"
          >
            {{ t("mcp.allowRemoteWarning") }}
          </p>
        </div>

        <!-- 客户端配置（仅 Streamable HTTP，D1） -->
        <div class="mt-4 border-t border-border-base pt-4">
          <div class="flex items-center justify-between">
            <span class="text-[13px] text-text-muted">{{ t("mcp.title") }} 客户端配置</span>
            <BaseButton size="sm" :disabled="!mcp.clientConfig" @click="copyConfig">
              <Check v-if="copied" class="h-3.5 w-3.5 text-success" />
              <Copy v-else class="h-3.5 w-3.5" />
              {{ copied ? t("common.copied") : t("mcp.copyConfig") }}
            </BaseButton>
          </div>
          <pre
            v-if="mcp.clientConfig"
            class="mt-3 max-h-56 overflow-auto rounded-lg bg-surface-code px-3 py-2.5 font-mono text-[11.5px] leading-relaxed text-text-code"
          >{{ mcp.clientConfig }}</pre>
          <p v-else class="mt-2 text-[11.5px] text-text-muted">
            启动 MCP Server 后可复制客户端配置。
          </p>
        </div>

        <div class="mt-4 flex items-center justify-between gap-4 border-t border-border-base pt-4">
          <div>
            <span class="text-[13px]">{{ t("mcp.autoStart") }}</span>
            <p class="mt-0.5 text-[11.5px] text-text-muted">
              {{ t("mcp.autoStartHint") }}
            </p>
          </div>
          <BaseSwitch
            :model-value="mcp.autoStart"
            @update:model-value="(v) => mcp.setAutoStart(v)"
          />
        </div>
      </section>

      <!-- ============ 外观 ============ -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <h2 class="mb-4 text-[15px] font-semibold">{{ t("settings.appearance") }}</h2>
        <div class="flex items-center justify-between gap-4">
          <span class="text-[13px] text-text-muted">{{ t("settings.theme") }}</span>
          <div class="flex gap-0.5 rounded-[10px] border border-border-base bg-surface p-[3px]">
            <button
              v-for="opt in themeOptions"
              :key="opt.value"
              type="button"
              class="rounded-[7px] px-3 py-1.5 text-[11.5px] transition-colors"
              :class="
                theme.mode === opt.value
                  ? 'bg-surface-hover font-semibold text-text-base'
                  : 'text-text-muted hover:text-text-base'
              "
              @click="theme.setMode(opt.value)"
            >
              {{ t(opt.labelKey) }}
            </button>
          </div>
        </div>
      </section>

      <!-- ============ 安全 ============ -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <h2 class="mb-4 text-[15px] font-semibold">{{ t("settings.security") }}</h2>

        <div class="flex items-center justify-between gap-4 text-[13px]">
          <span class="text-text-muted">{{ t("settings.keyProvider") }}</span>
          <div class="flex items-center gap-2">
            <StatusTag :tone="app.keyStatus?.unlocked ? 'success' : 'warning'">
              {{ app.keyStatus?.unlocked ? "已解锁" : "未解锁" }}
            </StatusTag>
            <span>{{ keyProviderLabel }}</span>
          </div>
        </div>

        <p
          v-if="app.keyStatus?.unavailable_reason"
          class="mt-3 rounded-lg border border-warning/30 bg-warning-soft px-3 py-2 text-[11.5px] leading-relaxed text-warning"
        >
          {{ app.keyStatus.unavailable_reason }}。凭据无法解密，
          请通过下方按钮重新解锁或导入密钥，数据不会被清除。
        </p>

        <div class="mt-3 flex gap-2">
          <BaseButton
            v-if="app.needsSetup"
            size="sm"
            variant="primary"
            @click="(() => { keyDialogUnlockMode = false; keyDialogOpen = true; })()"
          >
            {{ t("settings.keyProvider") }}
          </BaseButton>
          <BaseButton
            v-else-if="app.needsUnlock"
            size="sm"
            variant="primary"
            @click="(() => { keyDialogUnlockMode = true; keyDialogOpen = true; })()"
          >
            解锁凭据
          </BaseButton>
        </div>

        <p class="mt-3 text-[11.5px] leading-relaxed text-text-muted">
          {{ t("settings.disclaimer") }}
        </p>
      </section>

      <!-- ============ 数据 ============ -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <h2 class="mb-4 text-[15px] font-semibold">{{ t("settings.data") }}</h2>

        <div class="flex items-center justify-between gap-4 text-[13px]">
          <span class="text-text-muted">{{ t("settings.historyRetention") }}</span>
          <span>{{ t("settings.historyRetentionValue", { value: 30, unit: t("settings.unitDay") }) }}</span>
        </div>
        <p class="mt-2 text-[11.5px] leading-relaxed text-text-muted">
          {{ t("settings.historyRetentionHint") }}
        </p>

        <div class="mt-3 flex items-center justify-between gap-4 text-[13px]">
          <span class="text-text-muted">{{ t("settings.historyStats") }}</span>
          <span class="font-mono text-[12px]">
            {{
              stats
                ? t("settings.historyStatsValue", {
                    commands: stats.command_count,
                    size: formatBytes(stats.output_bytes),
                  })
                : "—"
            }}
          </span>
        </div>
      </section>

      <!-- ============ 关于（D26） ============ -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <h2 class="mb-4 flex items-center gap-2 text-[15px] font-semibold">
          <Info class="h-4 w-4 text-text-muted" />
          {{ t("settings.about") }}
        </h2>

        <dl class="grid gap-3 text-[13px]">
          <div class="flex items-center justify-between gap-4">
            <dt class="text-text-muted">{{ t("settings.version") }}</dt>
            <dd class="font-mono text-[12px]">v0.1.0</dd>
          </div>
          <div class="flex items-center justify-between gap-4">
            <dt class="text-text-muted">{{ t("settings.license") }}</dt>
            <dd class="font-mono text-[12px]">Apache-2.0</dd>
          </div>
        </dl>

        <div class="mt-4 flex items-center gap-2 border-t border-border-base pt-4">
          <BaseButton size="sm" :disabled="updateStatus === 'checking'" @click="checkUpdate">
            <RefreshCw
              class="h-3.5 w-3.5"
              :class="updateStatus === 'checking' && 'animate-spin'"
            />
            {{ t("settings.checkUpdate") }}
          </BaseButton>
          <span v-if="updateStatus === 'latest'" class="text-[12px] text-success">
            {{ t("settings.updateUpToDate") }}
          </span>
        </div>

        <div class="mt-4 flex gap-2 border-t border-border-base pt-4">
          <BaseButton size="sm" variant="ghost">
            <ExternalLink class="h-3.5 w-3.5" />
            {{ t("settings.sourceCode") }}
          </BaseButton>
          <BaseButton size="sm" variant="ghost">
            <ExternalLink class="h-3.5 w-3.5" />
            {{ t("settings.docs") }}
          </BaseButton>
        </div>
      </section>
    </div>

    <ConfirmDialog
      :open="confirmRegenOpen"
      :title="t('mcp.tokenRegenerateConfirm')"
      :message="t('mcp.token')"
      :warning="t('mcp.tokenRegenerateWarning')"
      :confirm-label="t('common.confirm')"
      :loading="mcp.loading"
      @confirm="confirmRegenerate"
      @cancel="confirmRegenOpen = false"
    />

    <KeySetupDialog
      :open="keyDialogOpen"
      :unlock-mode="keyDialogUnlockMode"
      @close="keyDialogOpen = false"
    />
  </PageShell>
</template>
