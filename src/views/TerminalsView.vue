<script setup lang="ts">
/**
 * 终端与审计（合并页，D26）。
 *
 * 两种入口：
 * 1. 分组浏览——按主机查看看该主机下的终端
 * 2. 全局搜索——跨终端检索命令与输出（Q17）
 *
 * 权限边界：人类只读，可归档 / 恢复 / 删除，但**无法在终端执行命令**。
 */
import { ref } from "vue";
import { useI18n } from "vue-i18n";
import { TerminalSquare, Search, Archive, Trash2 } from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import EmptyState from "@/components/ui/EmptyState.vue";

const { t } = useI18n();

const mode = ref<"browse" | "search">("browse");
const searchQuery = ref("");

// 阶段一：数据源尚未接入 Rust 侧。
const terminals: Array<{
  id: string;
  name: string | null;
  hostName: string;
  status: "active" | "broken" | "archived";
  lastActivity: string;
  commandCount: number;
}> = [];

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
</script>

<template>
  <PageShell :title="t('terminal.title')" :icon="TerminalSquare">
    <template #actions>
      <div class="flex gap-0.5 rounded-[10px] border border-border-base bg-surface p-[3px]">
        <button
          type="button"
          class="rounded-[7px] px-3 py-1.5 text-[11.5px] text-text-muted transition-colors"
          :class="mode === 'browse' && 'bg-surface-hover font-semibold text-text-base'"
          @click="mode = 'browse'"
        >
          {{ t('command.browseMode') }}
        </button>
        <button
          type="button"
          class="rounded-[7px] px-3 py-1.5 text-[11.5px] text-text-muted transition-colors"
          :class="mode === 'search' && 'bg-surface-hover font-semibold text-text-base'"
          @click="mode = 'search'"
        >
          <Search class="mr-1 inline h-3.5 w-3.5" />
          {{ t('command.searchMode') }}
        </button>
      </div>
    </template>

    <!-- 全局搜索入口（Q17） -->
    <div v-if="mode === 'search'" class="mb-4">
      <div class="relative">
        <Search
          class="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-text-muted"
        />
        <input
          v-model="searchQuery"
          type="text"
          :placeholder="t('command.searchPlaceholder')"
          class="w-full rounded-[10px] border border-border-base bg-surface py-2.5 pl-9 pr-3 text-[13px] text-text-base outline-none transition-colors placeholder:text-text-muted focus:border-accent"
        />
      </div>
    </div>

    <EmptyState
      v-if="terminals.length === 0"
      :icon="TerminalSquare"
      :title="mode === 'search' ? t('command.noResults') : t('terminal.empty')"
      :hint="mode === 'search' ? undefined : t('terminal.emptyHint')"
    />

    <div v-else class="grid gap-3">
      <article
        v-for="term in terminals"
        :key="term.id"
        class="rounded-xl border border-border-base bg-surface p-4 transition-colors hover:bg-surface-hover"
      >
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <h3 class="truncate text-[14px] font-semibold">
              {{ term.name || t("terminal.unnamed") }}
            </h3>
            <p class="mt-0.5 truncate font-mono text-[11.5px] text-text-muted">
              {{ term.id }} · {{ term.hostName }}
            </p>
          </div>
          <StatusTag :tone="statusTone(term.status)">{{ statusLabel(term.status) }}</StatusTag>
        </div>

        <div class="mt-3 flex flex-wrap items-center gap-2">
          <StatusTag tone="neutral">{{ term.commandCount }} commands</StatusTag>
          <span class="text-[11.5px] text-text-muted">{{ term.lastActivity }}</span>
          <div class="ml-auto flex gap-1.5">
            <BaseButton size="sm" variant="ghost" :title="t('terminal.archive')">
              <Archive class="h-3.5 w-3.5" />
            </BaseButton>
            <BaseButton size="sm" variant="ghost" :title="t('common.delete')">
              <Trash2 class="h-3.5 w-3.5" />
            </BaseButton>
          </div>
        </div>
      </article>
    </div>
  </PageShell>
</template>
