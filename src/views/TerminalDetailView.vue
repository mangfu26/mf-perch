<script setup lang="ts">
/**
 * 终端详情：命令时间线审计视图（D25 / Q23）。
 *
 * 采用时间线卡片而非终端回放：人类在本产品中是"日志审核者"，
 * 每条命令的边界、退出码与耗时比终端观感更重要。
 */
import { ref, computed } from "vue";
import { useI18n } from "vue-i18n";
import { useRoute, useRouter } from "vue-router";
import {
  ArrowLeft,
  Archive,
  RotateCcw,
  Trash2,
  ChevronDown,
  ChevronRight,
  ShieldAlert,
  TerminalSquare,
} from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import { formatDuration, formatDateTime } from "@/lib/format";

const { t } = useI18n();
const route = useRoute();
const router = useRouter();

/** 终端状态取值与 Rust 侧 `TerminalStatus` 对应。 */
type TerminalStatus = "active" | "broken" | "archived";
type CommandStatus = "queued" | "running" | "completed" | "failed";
type SudoMode = "none" | "injected" | "asked" | "denied";

interface TerminalDetail {
  id: string;
  name: string | null;
  hostName: string;
  status: TerminalStatus;
  createdAt: string;
  lastActivity: string;
}

interface CommandItem {
  id: string;
  seq: number;
  command: string;
  status: CommandStatus;
  exitCode: number | null;
  durationMs: number | null;
  truncated: boolean;
  output: string;
  createdAt: string;
  sudoMode: SudoMode;
}

// 阶段一：数据源尚未接入 Rust 侧 IPC，先以空值驱动界面结构。
const terminal = ref<TerminalDetail | null>(null);
const commands = ref<CommandItem[]>([]);

// 路由参数在阶段二用于向 Rust 侧查询该终端。
void route;

/** 展开的命令 ID 集合。 */
const expanded = ref<Set<string>>(new Set());

function toggle(id: string) {
  const next = new Set(expanded.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  expanded.value = next;
}

const isExpanded = (id: string) => expanded.value.has(id);

function statusTone(status: CommandStatus) {
  if (status === "completed") return "success";
  if (status === "running") return "info";
  if (status === "queued") return "neutral";
  return "danger";
}

function statusLabel(status: CommandStatus) {
  if (status === "queued") return t("command.statusQueued");
  if (status === "running") return t("command.statusRunning");
  if (status === "failed") return t("command.statusFailed");
  return t("command.statusCompleted");
}

function sudoLabel(mode: SudoMode) {
  if (mode === "injected") return t("command.sudoInjected");
  if (mode === "asked") return t("command.sudoAsked");
  if (mode === "denied") return t("command.sudoDenied");
  return null;
}

const canRestore = computed(() => terminal.value?.status === "archived");
</script>

<template>
  <PageShell
    :title="terminal?.name || t('terminal.unnamed')"
    :subtitle="terminal ? `${terminal.id} · ${terminal.hostName}` : undefined"
    :icon="TerminalSquare"
  >
    <template #actions>
      <BaseButton variant="ghost" @click="router.push('/terminals')">
        <ArrowLeft class="h-3.5 w-3.5" />
        {{ t("common.back") }}
      </BaseButton>
      <BaseButton v-if="canRestore" variant="default">
        <RotateCcw class="h-3.5 w-3.5" />
        {{ t("terminal.restore") }}
      </BaseButton>
      <BaseButton v-else variant="default">
        <Archive class="h-3.5 w-3.5" />
        {{ t("terminal.archive") }}
      </BaseButton>
      <BaseButton variant="danger">
        <Trash2 class="h-3.5 w-3.5" />
        {{ t("common.delete") }}
      </BaseButton>
    </template>

    <!-- 状态提示：归档 / 断开都需要明确告知人类（D20 / D3） -->
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

    <EmptyState v-if="commands.length === 0" :icon="TerminalSquare" :title="t('command.empty')" />

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
            <StatusTag v-if="sudoLabel(cmd.sudoMode)" tone="accent">
              <ShieldAlert class="h-3 w-3" />
              {{ sudoLabel(cmd.sudoMode) }}
            </StatusTag>
            <StatusTag :tone="statusTone(cmd.status)">{{ statusLabel(cmd.status) }}</StatusTag>
            <StatusTag
              v-if="cmd.exitCode !== null"
              :tone="cmd.exitCode === 0 ? 'success' : 'danger'"
            >
              exit {{ cmd.exitCode }}
            </StatusTag>
            <StatusTag tone="neutral">{{ formatDuration(cmd.durationMs) }}</StatusTag>
          </div>
        </button>

        <div v-if="isExpanded(cmd.id)">
          <pre
            class="max-h-[420px] overflow-auto bg-surface-code px-3.5 py-3 font-mono text-[12px] leading-[1.65] text-text-code"
          >{{ cmd.output }}</pre>
          <div
            class="flex items-center gap-3 border-t border-border-base px-3.5 py-2 text-[11px] text-text-muted"
          >
            <span>{{ formatDateTime(cmd.createdAt) }}</span>
            <span v-if="cmd.truncated" class="text-warning">{{ t("command.truncated") }}</span>
          </div>
        </div>
      </article>
    </div>
  </PageShell>
</template>
