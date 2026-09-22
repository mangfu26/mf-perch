<script setup lang="ts">
/**
 * 设置页（D26）：MCP、外观、安全、数据、关于。
 *
 * "关于"作为设置页内的分区，不占一级导航（D26）。
 * 安全相关项遵循 P2：降级必须显式告知。
 */
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import {
  Settings,
  Power,
  Copy,
  RefreshCw,
  RotateCcw,
  Eye,
  EyeOff,
  ExternalLink,
  Info,
  Check,
} from "lucide-vue-next";
import { openUrl } from "@tauri-apps/plugin-opener";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import BaseInput from "@/components/ui/BaseInput.vue";
import BaseSwitch from "@/components/ui/BaseSwitch.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import JsonCodeBlock from "@/components/ui/JsonCodeBlock.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import KeySetupDialog from "@/components/settings/KeySetupDialog.vue";
import { useThemeStore, type ThemeMode } from "@/stores/theme";
import { useMcpStore } from "@/stores/mcp";
import { useAppStore } from "@/stores/app";
import { useUpdateStore } from "@/stores/update";
import { historyStats, runtimeSettings, setRuntimeSetting } from "@/lib/commands";
import { copyToClipboard, formatBytes } from "@/lib/format";
import type { HistoryStats } from "@/lib/api";

const { t } = useI18n();
const theme = useThemeStore();
const mcp = useMcpStore();
const app = useAppStore();
const update = useUpdateStore();

const showToken = ref(false);
const copied = ref(false);
const showRemoteWarning = ref(false);
const confirmRegenOpen = ref(false);
const keyDialogOpen = ref(false);
const keyDialogUnlockMode = ref(false);

const stats = ref<HistoryStats | null>(null);
/** 更新源地址输入框；与 store 同步。 */
const updateSource = ref("");

// ---- 运行期设置的本地编辑状态（Q11 / Q12 / Q4） ----
const quotaPerHost = ref(5);
const quotaGlobal = ref(20);
/** 保留期的数值与单位；单位为 `forever` 时数值不参与计算。 */
const retentionValue = ref(30);
const retentionUnit = ref<"hour" | "day" | "week" | "month" | "forever">("day");

/** 各单位对应的小时数。 */
const UNIT_HOURS: Record<string, number> = {
  hour: 1,
  day: 24,
  week: 24 * 7,
  month: 24 * 30,
};

/** 把保留期小时数拆成"数值 + 单位"，用于回填输入框。 */
function splitRetention(hours: number) {
  if (hours === 0) {
    retentionUnit.value = "forever";
    retentionValue.value = 0;
    return;
  }
  if (hours % UNIT_HOURS.month === 0) {
    retentionUnit.value = "month";
    retentionValue.value = hours / UNIT_HOURS.month;
  } else if (hours % UNIT_HOURS.week === 0) {
    retentionUnit.value = "week";
    retentionValue.value = hours / UNIT_HOURS.week;
  } else if (hours % UNIT_HOURS.day === 0) {
    retentionUnit.value = "day";
    retentionValue.value = hours / UNIT_HOURS.day;
  } else {
    retentionUnit.value = "hour";
    retentionValue.value = hours;
  }
}

/** 从运行期设置回填表单。 */
async function loadRuntimeSettings() {
  try {
    const s = await runtimeSettings();
    quotaPerHost.value = s.quota_per_host;
    quotaGlobal.value = s.quota_global;
    splitRetention(s.retention_hours);
  } catch {
    // 读取失败时保留默认值，不阻断设置页其他功能。
  }
}

/**
 * 保存保留期。
 *
 * 后端会做范围夹紧（最小 1 小时），因此用返回的生效值回填——
 * 让用户看到实际结果，而不是以为自己的输入已被采纳。
 */
async function saveRetention() {
  const hours =
    retentionUnit.value === "forever"
      ? 0
      : Math.round(Number(retentionValue.value) || 0) * UNIT_HOURS[retentionUnit.value];

  try {
    const effective = await setRuntimeSetting("history_retention_hours", String(hours));
    splitRetention(Number(effective) || 0);
  } catch (e) {
    app.fail(e);
    await loadRuntimeSettings();
  }
}

async function saveQuotaPerHost() {
  try {
    const effective = await setRuntimeSetting("quota_per_host", String(quotaPerHost.value));
    quotaPerHost.value = Number(effective) || 1;
    // 全局配额可能被自动抬升，需同步刷新显示。
    await loadRuntimeSettings();
  } catch (e) {
    app.fail(e);
  }
}

async function saveQuotaGlobal() {
  try {
    const effective = await setRuntimeSetting("quota_global", String(quotaGlobal.value));
    quotaGlobal.value = Number(effective) || 1;
    await loadRuntimeSettings();
  } catch (e) {
    app.fail(e);
  }
}

watch(
  () => update.info?.source_url,
  (url) => {
    if (url !== undefined) updateSource.value = url;
  },
  { immediate: true },
);

async function saveUpdateSource() {
  await update.setSource(updateSource.value);
}

/** 恢复内置默认更新源（清空自定义值即为回退，见 D42）。 */
async function resetUpdateSource() {
  await update.resetSource();
  // 输入框同步回显生效值（refresh 后 info.source_url 已是内置默认地址）。
  if (update.info?.source_url !== undefined) {
    updateSource.value = update.info.source_url;
  }
}

/**
 * 在系统浏览器里打开外部链接（D23：应用从不自动弹浏览器，一律由用户点击触发）。
 *
 * 更新提示传的是**该版本的 Release 页**而不是安装包直链（D58）：
 * 同一个 Release 下并列 msi 与 setup.exe，将来还有别的平台，选哪个由用户决定。
 */
async function openExternal(url: string) {
  try {
    await openUrl(url);
  } catch (e) {
    app.fail(e);
  }
}

/**
 * 源码仓库地址（关于卡片）。
 *
 * 只放这一个外链：**不提供"文档"链接**——本项目没有、也不打算建文档站点
 * （见 D41），站内也没有可发布的在线文档。若将来改变主意，直接在此追加常量。
 */
const REPO_URL = "https://github.com/mangfu26/mf-perch";

const themeOptions: Array<{ value: ThemeMode; labelKey: string }> = [
  { value: "system", labelKey: "settings.themeSystem" },
  { value: "dark", labelKey: "settings.themeDark" },
  { value: "light", labelKey: "settings.themeLight" },
];

onMounted(async () => {
  await Promise.all([mcp.refresh(), mcp.loadClientConfig(), update.refresh()]);
  await loadRuntimeSettings();
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
</script>

<template>
  <!-- width="narrow"：设置页整页共享一个居中限宽列（标题与卡片同轴），
       宽度由 PageShell 统一控制，此处不再自行限宽（见 theme-spec.md §4.2）。 -->
  <PageShell :title="t('settings.title')" :icon="Settings" width="narrow">
    <div class="flex flex-col gap-6 pb-8">
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
          <JsonCodeBlock v-if="mcp.clientConfig" :json="mcp.clientConfig" />
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

      <!-- ============ 数据与限额（Q11 / Q12 / Q4） ============ -->
      <section class="rounded-xl border border-border-base bg-surface p-5">
        <h2 class="mb-4 text-[15px] font-semibold">{{ t("settings.data") }}</h2>

        <!-- 历史保留期：值可配置，0 视为永久 -->
        <div class="flex items-center justify-between gap-4 text-[13px]">
          <span class="text-text-muted">{{ t("settings.historyRetention") }}</span>
          <div class="flex items-center gap-2">
            <BaseInput
              v-model="retentionValue"
              type="number"
              class="w-24"
              mono
              @change="saveRetention"
            />
            <BaseInput v-model="retentionUnit" as="select" class="w-24" @change="saveRetention">
              <option value="hour">{{ t("settings.unitHour") }}</option>
              <option value="day">{{ t("settings.unitDay") }}</option>
              <option value="week">{{ t("settings.unitWeek") }}</option>
              <option value="month">{{ t("settings.unitMonth") }}</option>
              <option value="forever">{{ t("settings.historyRetentionForever") }}</option>
            </BaseInput>
          </div>
        </div>
        <p class="mt-2 text-[11.5px] leading-relaxed text-text-muted">
          {{ t("settings.historyRetentionHint") }}
        </p>

        <!-- 终端配额（Q11） -->
        <div class="mt-4 grid grid-cols-2 gap-3">
          <div>
            <span class="mb-1.5 block text-[12.5px] text-text-muted">
              {{ t("settings.quotaPerHost") }}
            </span>
            <BaseInput
              v-model="quotaPerHost"
              type="number"
              mono
              @change="saveQuotaPerHost"
            />
          </div>
          <div>
            <span class="mb-1.5 block text-[12.5px] text-text-muted">
              {{ t("settings.quotaGlobal") }}
            </span>
            <BaseInput
              v-model="quotaGlobal"
              type="number"
              mono
              @change="saveQuotaGlobal"
            />
          </div>
        </div>
        <p class="mt-2 text-[11.5px] leading-relaxed text-text-muted">
          {{ t("settings.quotaHint") }}
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
            <dd class="font-mono text-[12px]">v{{ update.currentVersion }}</dd>
          </div>
          <div class="flex items-center justify-between gap-4">
            <dt class="text-text-muted">{{ t("settings.license") }}</dt>
            <dd class="font-mono text-[12px]">Apache-2.0</dd>
          </div>
        </dl>

        <!-- 检查更新（D23）：不自动下载，只提示并跳转下载页 -->
        <div class="mt-4 border-t border-border-base pt-4">
          <div class="flex items-center gap-2">
            <BaseButton size="sm" :disabled="update.checking" @click="update.check(true)">
              <RefreshCw class="h-3.5 w-3.5" :class="update.checking && 'animate-spin'" />
              {{ t("settings.checkUpdate") }}
            </BaseButton>
            <span
              v-if="update.result?.status === 'up_to_date'"
              class="text-[12px] text-success"
            >
              {{ t("settings.updateUpToDate") }}
            </span>
          </div>

          <!-- 有新版本：展示说明、下载入口与校验值 -->
          <div
            v-if="update.result?.status === 'available'"
            class="mt-3 rounded-[10px] border border-accent/30 bg-accent-soft px-3.5 py-3"
          >
            <p class="text-[13px] font-medium">
              {{ t("settings.updateAvailable", { version: update.result.latest }) }}
            </p>
            <p
              v-if="update.result.notes"
              class="mt-1.5 whitespace-pre-wrap text-[12px] leading-relaxed text-text-muted"
            >{{ update.result.notes }}</p>

            <div class="mt-2.5 flex flex-wrap items-center gap-2">
              <BaseButton
                v-if="update.result.release_url"
                size="sm"
                variant="primary"
                @click="openExternal(update.result.release_url)"
              >
                <ExternalLink class="h-3.5 w-3.5" />
                {{ t("settings.updateDownload") }}
              </BaseButton>
              <span v-else class="text-[11.5px] text-text-muted">
                {{ t("settings.updateNoAsset") }}
              </span>
              <BaseButton
                size="sm"
                variant="ghost"
                @click="update.ignoreVersion(update.result.latest)"
              >
                {{ t("settings.updateIgnore") }}
              </BaseButton>
            </div>

            <!-- 校验值只对应**本平台**那个安装包：发布页上并列着好几个文件，
                 不写清楚会被拿去核对另一个包。 -->
            <p
              v-if="update.result.sha256"
              class="mt-2.5 break-all font-mono text-[10.5px] text-text-muted"
            >
              {{ t("settings.updateSha256") }} {{ update.result.sha256 }}
            </p>
          </div>

          <!-- 手动检查失败：明确告知原因（自动检查静默，不会走到这里） -->
          <p
            v-if="update.manualError"
            class="mt-3 rounded-lg border border-warning/30 bg-warning-soft px-3 py-2 text-[11.5px] leading-relaxed text-warning"
          >
            {{ update.manualError }}
          </p>

          <div class="mt-3 flex items-center justify-between gap-4">
            <span class="text-[11.5px] text-text-muted">{{ t("settings.autoCheckUpdate") }}</span>
            <BaseSwitch
              :model-value="update.info?.auto_check ?? true"
              @update:model-value="(v) => update.setAutoCheck(v)"
            />
          </div>
        </div>

        <!-- 更新源地址：可配置，便于换源或指向镜像（D23） -->
        <details class="mt-3 border-t border-border-base pt-4">
          <summary class="cursor-pointer text-[12px] text-text-muted">
            {{ t("settings.updateSource") }}
          </summary>
          <div class="mt-2.5 flex gap-2">
            <BaseInput
              v-model="updateSource"
              class="flex-1"
              mono
              :placeholder="t('settings.updateSourcePlaceholder')"
            />
            <BaseButton size="sm" @click="saveUpdateSource">
              {{ t("common.save") }}
            </BaseButton>
            <!-- 内置默认值可被覆盖；「恢复默认」= 清空自定义值（D42） -->
            <BaseButton
              size="sm"
              variant="ghost"
              :disabled="!update.info?.source_is_custom"
              :title="t('settings.updateSourceReset')"
              @click="resetUpdateSource"
            >
              <RotateCcw class="h-3.5 w-3.5" />
            </BaseButton>
          </div>
          <p class="mt-1.5 text-[11px] leading-relaxed text-text-muted">
            {{ t("settings.updateSourceHint") }}
          </p>
          <p
            v-if="update.info && !update.info.source_is_custom"
            class="mt-1 text-[11px] leading-relaxed text-text-muted"
          >
            {{ t("settings.updateSourceUsingDefault") }}
          </p>
        </details>

        <div class="mt-4 flex gap-2 border-t border-border-base pt-4">
          <BaseButton
            size="sm"
            variant="ghost"
            @click="openExternal(REPO_URL)"
          >
            <ExternalLink class="h-3.5 w-3.5" />
            {{ t("settings.sourceCode") }}
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
