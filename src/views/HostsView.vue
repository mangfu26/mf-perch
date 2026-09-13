<script setup lang="ts">
/**
 * SSH 主机管理（D26）。
 *
 * 权限边界（AGENTS.md 0.1）：主机由人类创建与管理，AI Agent 只读。
 */
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Server, Plus, KeyRound, ShieldAlert, Pencil, Trash2 } from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import HostFormDialog from "@/components/host/HostFormDialog.vue";
import { useHostsStore } from "@/stores/hosts";
import type { HostInput, HostSummary } from "@/lib/api";

const { t } = useI18n();
const store = useHostsStore();

const formOpen = ref(false);
const editing = ref<HostSummary | null>(null);
const saving = ref(false);

const confirmOpen = ref(false);
const deleting = ref<HostSummary | null>(null);
const removing = ref(false);

onMounted(() => store.refresh());

function openCreate() {
  editing.value = null;
  formOpen.value = true;
}

function openEdit(host: HostSummary) {
  editing.value = host;
  formOpen.value = true;
}

async function onSubmit(input: HostInput) {
  saving.value = true;
  try {
    await store.save(input);
    formOpen.value = false;
  } catch {
    // 错误已由 store 提示，保持对话框打开以便修正。
  } finally {
    saving.value = false;
  }
}

function askDelete(host: HostSummary) {
  deleting.value = host;
  confirmOpen.value = true;
}

async function confirmDelete() {
  if (!deleting.value) return;
  removing.value = true;
  try {
    await store.remove(deleting.value.id);
    confirmOpen.value = false;
  } catch {
    // 错误已提示。
  } finally {
    removing.value = false;
  }
}

function sudoLabel(policy: string) {
  if (policy === "ask") return t("host.sudoPolicyAsk");
  if (policy === "auto") return t("host.sudoPolicyAuto");
  return t("host.sudoPolicyDeny");
}
</script>

<template>
  <PageShell :title="t('host.title')" :icon="Server">
    <template #actions>
      <BaseButton variant="primary" @click="openCreate">
        <Plus class="h-3.5 w-3.5" />
        {{ t("host.add") }}
      </BaseButton>
    </template>

    <div v-if="store.loading && store.hosts.length === 0" class="text-[13px] text-text-muted">
      {{ t("common.loading") }}
    </div>

    <EmptyState
      v-else-if="store.hosts.length === 0"
      :icon="Server"
      :title="t('host.empty')"
      :hint="t('host.emptyHint')"
    >
      <BaseButton variant="primary" @click="openCreate">
        <Plus class="h-3.5 w-3.5" />
        {{ t("host.add") }}
      </BaseButton>
    </EmptyState>

    <!--
      卡片集合用响应式网格（theme-spec §4.2）：
      每张卡保持约 320px 以上的舒适宽度；窗口再宽也不会把单张卡拉成巨宽，
      操作按钮因此始终在视线与鼠标附近，而不是横跨整屏的右上角。
    -->
    <div
      v-else
      class="grid gap-3 grid-cols-[repeat(auto-fill,minmax(320px,1fr))]"
    >
      <article
        v-for="host in store.hosts"
        :key="host.id"
        class="group rounded-xl border border-border-base bg-surface p-4 transition-colors hover:bg-surface-hover"
      >
        <div class="flex items-start justify-between gap-3">
          <div class="flex min-w-0 items-start gap-2.5">
            <span
              class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
              :class="host.has_credential ? 'bg-success' : 'bg-text-muted'"
              :style="
                host.has_credential ? 'box-shadow:0 0 8px var(--success)' : undefined
              "
            />
            <div class="min-w-0">
              <h3 class="truncate text-[14px] font-semibold">
                {{ host.name || host.address }}
              </h3>
              <p class="mt-0.5 truncate font-mono text-[11.5px] text-text-muted">
                {{ host.address }}:{{ host.port }}
              </p>
            </div>
          </div>

          <div class="flex items-center gap-1.5">
            <div class="flex flex-wrap items-center gap-1.5">
              <StatusTag :tone="host.has_credential ? 'success' : 'warning'">
                <KeyRound class="h-3 w-3" />
                {{
                  host.has_credential
                    ? t("host.credential")
                    : t("host.credentialNone")
                }}
              </StatusTag>
              <StatusTag :tone="host.sudo_policy === 'deny' ? 'neutral' : 'accent'">
                <ShieldAlert class="h-3 w-3" />
                {{ sudoLabel(host.sudo_policy) }}
              </StatusTag>
            </div>
            <div class="flex gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
              <BaseButton size="sm" variant="ghost" @click="openEdit(host)">
                <Pencil class="h-3.5 w-3.5" />
              </BaseButton>
              <BaseButton size="sm" variant="ghost" @click="askDelete(host)">
                <Trash2 class="h-3.5 w-3.5" />
              </BaseButton>
            </div>
          </div>
        </div>

        <div class="mt-3 flex items-center gap-2">
          <StatusTag tone="info">
            {{ t("host.activeTerminals") }} {{ host.active_terminals }}
          </StatusTag>
          <StatusTag tone="neutral">
            {{ t("host.archivedTerminals") }} {{ host.archived_terminals }}
          </StatusTag>
        </div>
      </article>
    </div>

    <HostFormDialog
      :open="formOpen"
      :host="editing"
      :saving="saving"
      @submit="onSubmit"
      @close="formOpen = false"
    />

    <ConfirmDialog
      :open="confirmOpen"
      :title="t('host.deleteConfirm')"
      :message="deleting?.name || deleting?.address || ''"
      :warning="t('host.deleteWarning')"
      :confirm-label="t('common.delete')"
      danger
      :loading="removing"
      @confirm="confirmDelete"
      @cancel="confirmOpen = false"
    />
  </PageShell>
</template>
