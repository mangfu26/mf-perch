<script setup lang="ts">
/**
 * 终端详情：命令时间线审计视图（D25 / Q23）。
 *
 * 采用时间线卡片而非终端回放：人类在本产品中是"日志审核者"，
 * 每条命令的边界、退出码与耗时比终端观感更重要。
 *
 * 实时性（D22）：命令列表直接渲染 store 中的历史（**不做本地拷贝**），
 * 因此后端事件触发的 store 刷新会立即反映到界面；
 * 早期版本把 `store.history` 拷进本地 ref，导致"store 更新了、页面不动"，
 * 必须人工点刷新——这正是客户反馈的现象。
 */
import { ref, computed, onMounted, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useRoute, useRouter } from "vue-router";
import {
  ArrowLeft,
  Archive,
  RotateCcw,
  Trash2,
  ChevronDown,
  ChevronRight,
  TerminalSquare,
  RefreshCw,
} from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import { useTerminalsStore } from "@/stores/terminals";
import { formatDuration, formatDateTime } from "@/lib/format";

const { t } = useI18n();
const route = useRoute();
const router = useRouter();
const store = useTerminalsStore();

const terminalId = computed(() => String(route.params.id ?? ""));
const terminal = computed(() => store.findById(terminalId.value));

const commands = computed(() => store.history);
const loading = ref(false);
const expanded = ref<Set<string>>(new Set());
/** 默认展开只做一次：后续事件驱动的刷新不应重置用户已展开/折叠的状态。 */
let expandedInitialized = false;

const confirmOpen = ref(false);
const action = ref<"delete" | "archive" | "restore" | "reconnect">("delete");
const working = ref(false);

async function load() {
  loading.value = true;
  try {
    // 终端列表用于取详情（含归档终端，人类可审计）。
    if (store.terminals.length === 0) await store.refresh();
    await store.search({ terminalId: terminalId.value, limit: 500 });

    // 默认展开最新一条，便于快速查看最近发生了什么。
    if (!expandedInitialized && commands.value.length > 0) {
      expanded.value = new Set([commands.value[0].id]);
      expandedInitialized = true;
    }
  } finally {
    loading.value = false;
  }
}

onMounted(load);

// 切换终端（同一组件复用时参数会变）要重新取数，否则会继续显示上一个终端的历史。
watch(terminalId, load);

function toggle(id: string) {
  const next = new Set(expanded.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  expanded.value = next;
}

const isExpanded = (id: string) => expanded.value.has(id);

function commandStatusTone(status: string) {
  if (status === "completed") return "success";
  if (status === "running") return "info";
  if (status === "queued") return "neutral";
  return "danger";
}

function commandStatusLabel(status: string) {
  if (status === "queued") return t("command.statusQueued");
  if (status === "running") return t("command.statusRunning");
  if (status === "failed") return t("command.statusFailed");
  return t("command.statusCompleted");
}

const canRestore = computed(() => terminal.value?.status === "archived");
/** 会话断开（broken）时可手动重连：与 Agent 自动重连走同一条路径（D39）。 */
const canReconnect = computed(() => terminal.value?.status === "broken");

function ask(a: typeof action.value) {
  action.value = a;
  confirmOpen.value = true;
}

async function confirm() {
  const id = terminalId.value;
  working.value = true;
  try {
    if (action.value === "archive") await store.archive(id);
    else if (action.value === "restore") await store.restore(id);
    else if (action.value === "reconnect") await store.reconnect(id);
    else {
      await store.remove(id);
      router.push("/terminals");
      return;
    }
    await load();
    confirmOpen.value = false;
  } catch {
    // 错误已提示。
  } finally {
    working.value = false;
  }
}

const confirmTitle = computed(() => {
  if (action.value === "archive") return t("terminal.archiveConfirm");
  if (action.value === "restore") return t("terminal.restore");
  if (action.value === "reconnect") return t("terminal.reconnect");
  return t("terminal.deleteConfirm");
});

const confirmWarning = computed(() => {
  if (action.value === "archive") return t("terminal.archiveWarning");
  if (action.value === "restore") return t("terminal.restoreHint");
  if (action.value === "reconnect") return t("terminal.reconnectHint");
  return t("terminal.deleteWarning");
});
</script>

<template>
  <PageShell
    :title="terminal?.name || t('terminal.unnamed')"
    :subtitle="
      terminal
        ? `${terminal.id} · ${terminal.host_name || terminal.host_id}`
        : terminalId
    "
    :icon="TerminalSquare"
  >
    <template #actions>
      <BaseButton variant="ghost" @click="router.push('/terminals')">
        <ArrowLeft class="h-3.5 w-3.5" />
        {{ t("common.back") }}
      </BaseButton>
      <BaseButton variant="ghost" :title="t('common.refresh')" @click="load">
        <RefreshCw class="h-3.5 w-3.5" :class="loading && 'animate-spin'" />
      </BaseButton>
      <BaseButton
        v-if="canRestore"
        variant="primary"
        @click="ask('restore')"
      >
        <RotateCcw class="h-3.5 w-3.5" />
        {{ t("terminal.restore") }}
      </BaseButton>
      <BaseButton
        v-else-if="canReconnect"
        variant="primary"
        @click="ask('reconnect')"
      >
        <RotateCcw class="h-3.5 w-3.5" />
        {{ t("terminal.reconnect") }}
      </BaseButton>
      <BaseButton v-else variant="default" @click="ask('archive')">
        <Archive class="h-3.5 w-3.5" />
        {{ t("terminal.archive") }}
      </BaseButton>
      <BaseButton variant="danger" @click="ask('delete')">
        <Trash2 class="h-3.5 w-3.5" />
        {{ t("common.delete") }}
      </BaseButton>
    </template>

    <!-- 状态提示：归档 / 断开需明确告知人类（D20 / D3） -->
    <div
      v-if="terminal?.status === 'archived'"
      class="mb-4 rounded-[10px] border border-border-base bg-surface px-3.5 py-2.5 text-[12.5px] text-text-muted"
    >
      {{ t("terminal.archivedNote") }}
    </div>
    <div
      v-else-if="terminal?.status === 'broken'"
      class="mb-4 rounded-[10px] border border-danger/30 bg-danger-soft px-3.5 py-2.5 text-[12.5px] text-danger"
    >
      {{ t("terminal.brokenNote") }}
    </div>

    <!-- 环境快照：便于排查"命令找不到"（D4） -->
    <div
      v-if="terminal?.env_snapshot?.path"
      class="mb-4 rounded-[10px] border border-border-base bg-surface px-3.5 py-2.5"
    >
      <p class="mb-1 text-[11.5px] text-text-muted">
        环境快照（{{ terminal.env_snapshot.shell_env_mode === "login" ? t("host.shellEnvLogin") : t("host.shellEnvClean") }}）
      </p>
      <p class="truncate font-mono text-[11px] text-text-code">
        PATH={{ terminal.env_snapshot.path }}
      </p>
    </div>

    <div v-if="loading && commands.length === 0" class="text-[13px] text-text-muted">
      {{ t("common.loading") }}
    </div>

    <EmptyState
      v-else-if="commands.length === 0"
      :icon="TerminalSquare"
      :title="t('command.empty')"
    />

    <!-- 命令时间线 -->
    <div v-else class="flex flex-col gap-2.5">
      <article
        v-for="cmd in commands"
        :key="cmd.id"
        class="overflow-hidden rounded-xl border border-border-base bg-surface [box-shadow:var(--shadow-card)]"
      >
        <button
          type="button"
          class="flex w-full items-center gap-2.5 border-b border-border-base bg-surface-hover px-3.5 py-2.5 text-left"
          @click="toggle(cmd.id)"
        >
          <component
            :is="isExpanded(cmd.id) ? ChevronDown : ChevronRight"
            class="h-3.5 w-3.5 shrink-0 text-text-muted"
          />
          <code class="min-w-0 flex-1 truncate font-mono text-[12.5px]">
            <span class="text-accent">$</span> {{ cmd.command }}
          </code>
          <div class="flex shrink-0 items-center gap-1.5">
            <StatusTag :tone="commandStatusTone(cmd.status)">
              {{ commandStatusLabel(cmd.status) }}
            </StatusTag>
            <StatusTag
              v-if="cmd.exit_code !== null"
              :tone="cmd.exit_code === 0 ? 'success' : 'danger'"
            >
              exit {{ cmd.exit_code }}
            </StatusTag>
            <StatusTag tone="neutral">{{ formatDuration(cmd.duration_ms) }}</StatusTag>
          </div>
        </button>

        <div v-if="isExpanded(cmd.id)">
          <pre
            class="max-h-[420px] overflow-auto bg-surface-code px-3.5 py-3 font-mono text-[12px] leading-[1.65] text-text-code"
          >{{ cmd.output || t("command.output") }}</pre>
          <div
            class="flex items-center gap-3 border-t border-border-base px-3.5 py-2 text-[11px] text-text-muted"
          >
            <span>{{ formatDateTime(cmd.created_at) }}</span>
            <span v-if="cmd.truncated" class="text-warning">
              {{ t("command.truncated") }}
            </span>
          </div>
        </div>
      </article>
    </div>

    <ConfirmDialog
      :open="confirmOpen"
      :title="confirmTitle"
      :message="terminal?.name || terminalId"
      :warning="confirmWarning"
      :confirm-label="action === 'delete' ? t('common.delete') : t('common.confirm')"
      :danger="action === 'delete'"
      :loading="working"
      @confirm="confirm"
      @cancel="confirmOpen = false"
    />
  </PageShell>
</template>
