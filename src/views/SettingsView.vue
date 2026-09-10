<script setup lang="ts">
/**
 * 设置页（D26）：含 MCP、外观、安全、数据、更新与"关于"。
 *
 * "关于"作为设置页内的分区，不占一级导航（D26）。
 */
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import {
  Settings,
  Power,
  Copy,
  RefreshCw,
  Eye,
  EyeOff,
  Download,
  ExternalLink,
  Info,
} from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import { useThemeStore, type ThemeMode } from "@/stores/theme";
import { useMcpStore } from "@/stores/mcp";
import { copyToClipboard, formatBytes } from "@/lib/format";

const { t } = useI18n();
const theme = useThemeStore();
const mcp = useMcpStore();

const showToken = ref(false);
const copied = ref(false);
/** 安全降级提示的展开状态（P2：安全降级必须显式告知）。 */
const showRemoteWarning = ref(false);

const themeOptions: Array<{ value: ThemeMode; labelKey: string }> = [
  { value: "system", labelKey: "settings.themeSystem" },
  { value: "dark", labelKey: "settings.themeDark" },
  { value: "light", labelKey: "settings.themeLight" },
];

// 阶段一：以下配置项由 Rust 侧在阶段二接通。
const keyProvider = ref<"keyring" | "master_password" | "local_file">("keyring");
const retentionUnit = ref<"hour" | "day" | "week" | "month">("day");
const retentionValue = ref(30);
const historyStats = ref({ commands: 0, bytes: 0 });
const appVersion = ref("0.1.0");
const updateStatus = ref<"idle" | "checking" | "latest" | "available" | "failed">("idle");
const availableVersion = ref<string | null>(null);

const maskedToken = computed(() => {
  if (!mcp.token) return "—";
  if (showToken.value) return mcp.token;
  return "•".repeat(Math.min(mcp.token.length, 32));
});

async function copyConfig() {
  if (!mcp.clientConfig) return;
  const ok = await copyToClipboard(mcp.clientConfig);
  if (ok) {
    copied.value = true;
    setTimeout(() => (copied.value = false), 2000);
  }
}

function determineUnitLabel() {
  const map = {
    hour: "settings.unitHour",
    day: "settings.unitDay",
    week: "settings.unitWeek",
    month: "settings.unitMonth",
  } as const;
  return t(map[retentionUnit.value]);
}

async function checkUpdate() {
  updateStatus.value = "checking";
  // 阶段二接入真实的 Gist 更新源（D23）。
  updateStatus.value = "latest";
}

const retentionLabel = computed(() =>
  t("settings.historyRetentionValue", {
    value: retentionValue.value,
    unit: determineUnitLabel(),
  }),
);
</script>

<template>
  <PageShell :title="t('settings.title')" :icon="Settings">
    <div class="flex max-w-3xl flex-col gap-6 pb-8">
      <!-- MCP Server -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <div class="mb-4 flex items-center justify-between">
          <h2 class="text-[15px] font-semibold">{{ t("mcp.title") }}</h2>
          <BaseButton :variant="mcp.running ? 'default' : 'primary'" size="sm">
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
              <code class="max-w-[280px] truncate font-mono text-[12px]">{{ maskedToken }}</code>
              <BaseButton size="sm" variant="ghost" @click="showToken = !showToken">
                <component :is="showToken ? EyeOff : Eye" class="h-3.5 w-3.5" />
              </BaseButton>
              <BaseButton size="sm" variant="ghost">
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
          <label class="flex cursor-pointer items-center justify-between gap-4">
            <div>
              <span class="text-[13px]">{{ t("mcp.allowRemote") }}</span>
              <p class="mt-0.5 text-[11.5px] text-text-muted">{{ t("mcp.allowRemoteHint") }}</p>
            </div>
            <input
              v-model="mcp.allowRemote"
              type="checkbox"
              class="h-4 w-4 accent-[var(--accent)]"
              @change="showRemoteWarning = mcp.allowRemote"
            />
          </label>
          <p
            v-if="showRemoteWarning"
            class="mt-2 rounded-lg border border-warning/30 bg-warning-soft px-3 py-2 text-[11.5px] leading-relaxed text-warning"
          >
            {{ t("mcp.allowRemoteWarning") }}
          </p>
        </div>

        <div class="mt-4 border-t border-border-base pt-4">
          <BaseButton size="sm" @click="copyConfig">
            <Copy class="h-3.5 w-3.5" />
            {{ copied ? t("common.copied") : t("mcp.copyConfig") }}
          </BaseButton>
          <pre
            v-if="mcp.clientConfig"
            class="mt-3 max-h-56 overflow-auto rounded-lg bg-surface-code px-3 py-2.5 font-mono text-[11.5px] leading-relaxed text-text-code"
          >{{ mcp.clientConfig }}</pre>
        </div>
      </section>

      <!-- 外观 -->
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

      <!-- 安全 -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <h2 class="mb-4 text-[15px] font-semibold">{{ t("settings.security") }}</h2>
        <div class="flex items-center justify-between gap-4 text-[13px]">
          <span class="text-text-muted">{{ t("settings.keyProvider") }}</span>
          <StatusTag tone="success">
            {{
              keyProvider === "keyring"
                ? t("settings.keyProviderKeyring")
                : keyProvider === "master_password"
                  ? t("settings.keyProviderMasterPassword")
                  : t("settings.keyProviderLocalFile")
            }}
          </StatusTag>
        </div>
        <p class="mt-3 text-[11.5px] leading-relaxed text-text-muted">
          {{ t("settings.disclaimer") }}
        </p>
      </section>

      <!-- 数据 -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <h2 class="mb-4 text-[15px] font-semibold">{{ t("settings.data") }}</h2>
        <div class="flex items-center justify-between gap-4 text-[13px]">
          <span class="text-text-muted">{{ t("settings.historyRetention") }}</span>
          <span>{{ retentionLabel }}</span>
        </div>
        <p class="mt-2 text-[11.5px] leading-relaxed text-text-muted">
          {{ t("settings.historyRetentionHint") }}
        </p>
        <div class="mt-3 flex items-center justify-between gap-4 text-[13px]">
          <span class="text-text-muted">{{ t("settings.historyStats") }}</span>
          <span class="font-mono text-[12px]">
            {{
              t("settings.historyStatsValue", {
                commands: historyStats.commands,
                size: formatBytes(historyStats.bytes),
              })
            }}
          </span>
        </div>
      </section>

      <!-- 关于（含手动检查更新，D23 / D26） -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <h2 class="mb-4 flex items-center gap-2 text-[15px] font-semibold">
          <Info class="h-4 w-4 text-text-muted" />
          {{ t("settings.about") }}
        </h2>

        <dl class="grid gap-3 text-[13px]">
          <div class="flex items-center justify-between gap-4">
            <dt class="text-text-muted">{{ t("settings.version") }}</dt>
            <dd class="font-mono text-[12px]">v{{ appVersion }}</dd>
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
          <span v-else-if="updateStatus === 'available'" class="text-[12px] text-warning">
            {{ t("settings.updateAvailable", { version: availableVersion }) }}
          </span>
          <span v-else-if="updateStatus === 'failed'" class="text-[12px] text-danger">
            {{ t("settings.updateFailed") }}
          </span>
          <BaseButton v-if="updateStatus === 'available'" size="sm" variant="primary">
            <Download class="h-3.5 w-3.5" />
            {{ t("settings.updateDownload") }}
          </BaseButton>
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
  </PageShell>
</template>
