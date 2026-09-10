<script setup lang="ts">
/**
 * SSH 主机管理（D26）。
 *
 * 权限边界（AGENTS.md 0.1）：主机由人类创建与管理，AI Agent 只读。
 * 阶段一先呈现结构与绑定关系，增删改在阶段二接通 Rust 侧 IPC。
 */
import { useI18n } from "vue-i18n";
import { Server, Plus, KeyRound, ShieldAlert } from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import EmptyState from "@/components/ui/EmptyState.vue";

const { t } = useI18n();

// 阶段一：数据源尚未接入 Rust 侧，先以空列表驱动界面结构。
const hosts: Array<{
  id: string;
  name: string;
  address: string;
  port: number;
  hasCredential: boolean;
  sudoPolicy: "deny" | "ask" | "auto";
  activeTerminals: number;
  archivedTerminals: number;
}> = [];

function sudoLabel(policy: string) {
  if (policy === "ask") return t("host.sudoPolicyAsk");
  if (policy === "auto") return t("host.sudoPolicyAuto");
  return t("host.sudoPolicyDeny");
}
</script>

<template>
  <PageShell :title="t('host.title')" :icon="Server">
    <template #actions>
      <BaseButton variant="primary">
        <Plus class="h-3.5 w-3.5" />
        {{ t("host.add") }}
      </BaseButton>
    </template>

    <EmptyState
      v-if="hosts.length === 0"
      :icon="Server"
      :title="t('host.empty')"
      :hint="t('host.emptyHint')"
    >
      <BaseButton variant="primary">
        <Plus class="h-3.5 w-3.5" />
        {{ t("host.add") }}
      </BaseButton>
    </EmptyState>

    <div v-else class="grid gap-3">
      <article
        v-for="host in hosts"
        :key="host.id"
        class="rounded-xl border border-border-base bg-surface p-4 transition-colors hover:bg-surface-hover"
      >
        <div class="flex items-start justify-between gap-3">
          <div class="flex items-start gap-2.5">
            <span
              class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
              :class="host.hasCredential ? 'bg-success' : 'bg-text-muted'"
            />
            <div>
              <h3 class="text-[14px] font-semibold">
                {{ host.name || host.address }}
              </h3>
              <p class="mt-0.5 font-mono text-[11.5px] text-text-muted">
                {{ host.address }}:{{ host.port }}
              </p>
            </div>
          </div>
          <div class="flex items-center gap-1.5">
            <StatusTag :tone="host.hasCredential ? 'success' : 'warning'">
              <KeyRound class="h-3 w-3" />
              {{ host.hasCredential ? t("host.credential") : t("host.credentialNone") }}
            </StatusTag>
            <StatusTag :tone="host.sudoPolicy === 'deny' ? 'neutral' : 'accent'">
              <ShieldAlert class="h-3 w-3" />
              {{ sudoLabel(host.sudoPolicy) }}
            </StatusTag>
          </div>
        </div>
        <div class="mt-3 flex items-center gap-2">
          <StatusTag tone="info">
            {{ t("host.activeTerminals") }} {{ host.activeTerminals }}
          </StatusTag>
          <StatusTag tone="neutral">
            {{ t("host.archivedTerminals") }} {{ host.archivedTerminals }}
          </StatusTag>
        </div>
      </article>
    </div>
  </PageShell>
</template>
