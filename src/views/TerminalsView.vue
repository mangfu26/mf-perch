<script setup lang="ts">
/**
 * 终端与审计（合并页，D26）。
 *
 * 两种模式：
 * 1. 按主机浏览——查看某终端的状态与历史
 * 2. 全局搜索——跨终端检索命令与输出（Q17）
 *
 * 权限边界：人类只读，可归档 / 恢复 / 删除，**不能向终端输入命令**。
 */
import { onMounted, ref, computed, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useRouter } from "vue-router";
import {
  TerminalSquare,
  Search,
  Archive,
  RotateCcw,
  Unplug,
  Trash2,
  ChevronRight,
  ChevronDown,
  Clock,

} from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import BaseInput from "@/components/ui/BaseInput.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import { useTerminalsStore } from "@/stores/terminals";
import { useHostsStore } from "@/stores/hosts";
import { formatDuration, formatRelativeTime } from "@/lib/format";
import type { TerminalView } from "@/lib/api";

const { t } = useI18n();
const router = useRouter();
const store = useTerminalsStore();
const hosts = useHostsStore();

const mode = ref<"browse" | "search">("browse");
const searchQuery = ref("");
const filterHostId = ref("");
const showArchived = ref(true);

const confirmOpen = ref(false);
const target = ref<TerminalView | null>(null);
const action = ref<"delete" | "archive" | "restore" | "reconnect">("delete");
const working = ref(false);

onMounted(async () => {
  await Promise.all([store.refresh(), hosts.refresh()]);
  await store.search({ limit: 100 });
});

const visibleTerminals = computed(() =>
  showArchived.value
    ? store.terminals
    : store.terminals.filter((x) => x.status !== "archived"),
);

const filteredHistory = computed(() =>
  filterHostId.value
    ? store.history.filter((h) => {
        const term = store.findById(h.terminal_id);
        return term?.host_id === filterHostId.value;
      })
    : store.history,
);

/**
 * 全局搜索里命令卡片的展开态（问题 3：与终端详情页统一，支持收起）。
 * 默认全部收起——搜索结果往往一次几十条，全展开会把页面撑得极长；
 * 用户点开想看的那条即可，与详情页"默认只展开最新一条"的取向一致。
 */
const expandedCommands = ref<Set<string>>(new Set());

function toggleCommand(id: string) {
  const next = new Set(expandedCommands.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  expandedCommands.value = next;
}

const isCommandExpanded = (id: string) => expandedCommands.value.has(id);

// 结果集变化（重新搜索 / 切主机）时清掉展开态：旧命令 id 已不在列表里，
// 留着只是无意义的内存，且新结果应回到"全部收起"的一致起点。
watch(filteredHistory, () => {
  expandedCommands.value = new Set();
});

async function runSearch() {
  await store.search({
    query: searchQuery.value.trim() || undefined,
    limit: 200,
  });
}

function statusTone(status: string) {
  if (status === "active") return "success";
  if (status === "broken") return "danger";
  return "neutral";
}

function statusLabel(status: string) {
  if (status === "active") return t("terminal.statusActive");
  if (status === "broken") return t("terminal.statusBroken");
  return t("terminal.statusArchived");
}

function commandStatusTone(status: string) {
  if (status === "completed") return "success";
  // 连接断开：命令结局未知，不能混进 danger（那会被读成"命令失败"）。
  if (status === "connection_lost") return "warning";
  if (status === "running") return "info";
  if (status === "queued") return "neutral";
  return "danger";
}

function commandStatusLabel(status: string) {
  if (status === "queued") return t("command.statusQueued");
  if (status === "running") return t("command.statusRunning");
  if (status === "failed") return t("command.statusFailed");
  if (status === "connection_lost") return t("command.statusConnectionLost");
  return t("command.statusCompleted");
}

function ask(term: TerminalView, a: typeof action.value) {
  target.value = term;
  action.value = a;
  confirmOpen.value = true;
}

async function confirm() {
  if (!target.value) return;
  working.value = true;
  try {
    if (action.value === "archive") await store.archive(target.value.id);
    else if (action.value === "restore") await store.restore(target.value.id);
    else if (action.value === "reconnect") await store.reconnect(target.value.id);
    else await store.remove(target.value.id);
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
  if (action.value === "restore") return undefined;
  if (action.value === "reconnect") return t("terminal.reconnectHint");
  return t("terminal.deleteWarning");
});
</script>

<template>
  <PageShell :title="t('terminal.title')" :icon="TerminalSquare">
    <template #actions>
      <!-- 模式切换：按主机浏览 / 全局搜索（D26） -->
      <div class="flex gap-0.5 rounded-[10px] border border-border-base bg-surface p-[3px]">
        <button
          type="button"
          class="rounded-[7px] px-3 py-1.5 text-[11.5px] transition-colors"
          :class="
            mode === 'browse'
              ? 'bg-surface-hover font-semibold text-text-base'
              : 'text-text-muted hover:text-text-base'
          "
          @click="mode = 'browse'"
        >
          {{ t("command.browseMode") }}
        </button>
        <button
          type="button"
          class="rounded-[7px] px-3 py-1.5 text-[11.5px] transition-colors"
          :class="
            mode === 'search'
              ? 'bg-surface-hover font-semibold text-text-base'
              : 'text-text-muted hover:text-text-base'
          "
          @click="mode = 'search'"
        >
          {{ t("command.searchMode") }}
        </button>
      </div>
    </template>

    <!-- ============ 按主机浏览 ============ -->
    <template v-if="mode === 'browse'">
      <div class="mb-3 flex items-center gap-3">
        <label class="flex cursor-pointer items-center gap-2 text-[12.5px] text-text-muted">
          <input
            v-model="showArchived"
            type="checkbox"
            class="accent-[var(--accent)]"
          />
          {{ t("terminal.statusArchived") }}
        </label>
        <span class="text-[12px] text-text-muted">
          {{ visibleTerminals.length }} {{ t("terminal.title") }}
        </span>
      </div>

      <EmptyState
        v-if="visibleTerminals.length === 0"
        :icon="TerminalSquare"
        :title="t('terminal.empty')"
        :hint="t('terminal.emptyHint')"
      />

      <!-- 终端卡片集合：与主机/凭据页同款响应式网格（最小列宽 400px，theme-spec §4.2） -->
      <div
        v-else
        class="grid gap-3 grid-cols-[repeat(auto-fill,minmax(400px,1fr))]"
      >
        <article
          v-for="term in visibleTerminals"
          :key="term.id"
          class="group cursor-pointer rounded-xl border border-border-base bg-surface p-4 transition-colors hover:bg-surface-hover"
          @click="router.push(`/terminals/${term.id}`)"
        >
          <!-- 头部行只放「身份 + 导航指示」；状态标签移到元信息行（theme-spec §4.2），
               头部便不再与标签抢宽度，长终端名也不会被挤到换行 -->
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <h3 class="truncate text-[14px] font-semibold">
                {{ term.name || t("terminal.unnamed") }}
              </h3>
              <p class="mt-0.5 truncate font-mono text-[11.5px] text-text-muted">
                {{ term.id }} · {{ term.host_name || term.host_id }}
              </p>
            </div>
            <ChevronRight
              class="mt-0.5 h-4 w-4 shrink-0 text-text-muted opacity-0 transition-opacity group-hover:opacity-100"
            />
          </div>

          <div class="mt-3 flex flex-wrap items-center gap-2">
            <StatusTag :tone="statusTone(term.status)">
              {{ statusLabel(term.status) }}
            </StatusTag>
            <StatusTag tone="neutral">
              {{ term.command_count }} commands
            </StatusTag>
            <span class="flex items-center gap-1 text-[11.5px] text-text-muted">
              <Clock class="h-3 w-3" />
              {{ formatRelativeTime(term.updated_at) }}
            </span>

            <div
              class="ml-auto flex gap-1 opacity-0 transition-opacity group-hover:opacity-100"
              @click.stop
            >
              <BaseButton
                v-if="term.status === 'archived'"
                size="sm"
                variant="ghost"
                :title="t('terminal.restore')"
                :aria-label="t('terminal.restore')"
                @click="ask(term, 'restore')"
              >
                <RotateCcw class="h-3.5 w-3.5" />
              </BaseButton>
              <BaseButton
                v-else-if="term.status === 'broken'"
                size="sm"
                variant="ghost"
                :title="t('terminal.reconnect')"
                :aria-label="t('terminal.reconnect')"
                @click="ask(term, 'reconnect')"
              >
                <!-- 重连用 Unplug（断开→重新接通），与"恢复归档"的 RotateCcw 区分；
                     不用 RefreshCw 是因为详情页顶栏已用它做"刷新页面"，同屏会混。 -->
                <Unplug class="h-3.5 w-3.5" />
              </BaseButton>
              <BaseButton
                v-else
                size="sm"
                variant="ghost"
                :title="t('terminal.archive')"
                :aria-label="t('terminal.archive')"
                @click="ask(term, 'archive')"
              >
                <Archive class="h-3.5 w-3.5" />
              </BaseButton>
              <!-- 删除是破坏性操作：悬停转危险色，与确认弹窗的红色按钮呼应 -->
              <BaseButton
                size="sm"
                variant="ghost"
                tone="danger"
                :title="t('common.delete')"
                :aria-label="t('common.delete')"
                @click="ask(term, 'delete')"
              >
                <Trash2 class="h-3.5 w-3.5" />
              </BaseButton>
            </div>
          </div>
        </article>
      </div>
    </template>

    <!-- ============ 全局搜索（Q17） ============ -->
    <template v-else>
      <div class="mb-4 flex items-center gap-2">
        <!-- min-w-0：允许该 flex 项收缩到内容宽度以下，否则长主机名的下拉
             会把搜索框挤成图标宽的窄条（flex 子项默认 min-width:auto）。 -->
        <div class="relative min-w-0 flex-1">
          <Search
            class="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-text-muted"
          />
          <BaseInput
            v-model="searchQuery"
            class="pl-9"
            :placeholder="t('command.searchPlaceholder')"
          />
        </div>
        <!-- shrink-0：下拉与按钮不参与压缩，宽度由自身决定。 -->
        <BaseInput v-model="filterHostId" as="select" class="w-48 shrink-0">
          <option value="">{{ t("command.allHosts") }}</option>
          <option v-for="h in hosts.hosts" :key="h.id" :value="h.id">
            {{ hosts.label(h) }}
          </option>
        </BaseInput>
        <BaseButton variant="primary" class="shrink-0 whitespace-nowrap" @click="runSearch">
          {{ t("common.search") }}
        </BaseButton>
      </div>

      <EmptyState
        v-if="filteredHistory.length === 0"
        :icon="Search"
        :title="t('command.noResults')"
      />

      <div v-else class="flex flex-col gap-2.5">
        <article
          v-for="item in filteredHistory"
          :key="item.id"
          class="overflow-hidden rounded-xl border border-border-base bg-surface"
        >
          <!-- 头部整行可点：折叠 / 展开输出，与终端详情页统一（问题 3）。 -->
          <button
            type="button"
            class="flex w-full items-center gap-2.5 border-b border-border-base bg-surface-hover px-3.5 py-2.5 text-left"
            @click="toggleCommand(item.id)"
          >
            <component
              :is="isCommandExpanded(item.id) ? ChevronDown : ChevronRight"
              class="h-3.5 w-3.5 shrink-0 text-text-muted"
            />
            <code class="min-w-0 flex-1 truncate font-mono text-[12.5px]">
              <span class="text-accent">$</span> {{ item.command }}
            </code>
            <div class="flex shrink-0 items-center gap-1.5">
              <StatusTag :tone="commandStatusTone(item.status)">
                {{ commandStatusLabel(item.status) }}
              </StatusTag>
              <StatusTag
                v-if="item.exit_code !== null"
                :tone="item.exit_code === 0 ? 'success' : 'danger'"
              >
                exit {{ item.exit_code }}
              </StatusTag>
              <StatusTag tone="neutral">{{ formatDuration(item.duration_ms) }}</StatusTag>
            </div>
          </button>
          <pre
            v-if="isCommandExpanded(item.id) && item.output"
            class="max-h-52 overflow-auto bg-surface-code px-3.5 py-3 font-mono text-[12px] leading-[1.65] text-text-code"
          >{{ item.output }}</pre>
        </article>
      </div>
    </template>

    <ConfirmDialog
      :open="confirmOpen"
      :title="confirmTitle"
      :message="target?.name || target?.id || ''"
      :warning="confirmWarning"
      :confirm-label="action === 'delete' ? t('common.delete') : t('common.confirm')"
      :danger="action === 'delete'"
      :loading="working"
      @confirm="confirm"
      @cancel="confirmOpen = false"
    />
  </PageShell>
</template>
