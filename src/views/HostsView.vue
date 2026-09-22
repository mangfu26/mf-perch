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
  if (policy === "not_needed") return t("host.sudoPolicyNotNeeded");
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
      最小列宽取 **400px**——卡片头部要在一行内容纳「名称 + 两枚状态标签 +
      两个操作按钮」，实测需要约 380–400px；取 320px 时第二枚标签会换行、
      地址也会被过度截断（客户反馈）。
    -->
    <div
      v-else
      class="grid gap-3 grid-cols-[repeat(auto-fill,minmax(400px,1fr))]"
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

          <!--
            操作按钮独占右上角，**不与标签抢同一行**（theme-spec §4.2）：
            早先把两枚标签也塞在这一行，头部所需宽度涨到 ~455px，
            卡片一窄（例如两列布局时约 407px）第二枚标签就被挤到换行。
            删除用 tone="danger"：悬停转危险色，与确认弹窗的红色按钮连成一致的语义链。
          -->
          <div class="flex shrink-0 gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
            <BaseButton
              size="sm"
              variant="ghost"
              tone="accent"
              :title="t('host.edit')"
              :aria-label="t('host.edit')"
              @click="openEdit(host)"
            >
              <Pencil class="h-3.5 w-3.5" />
            </BaseButton>
            <BaseButton
              size="sm"
              variant="ghost"
              tone="danger"
              :title="t('common.delete')"
              :aria-label="t('common.delete')"
              @click="askDelete(host)"
            >
              <Trash2 class="h-3.5 w-3.5" />
            </BaseButton>
          </div>
        </div>

        <!-- 元信息行：状态标签与计数并排，允许换行（空间不足时整行下移，不挤压头部） -->
        <div class="mt-3 flex flex-wrap items-center gap-1.5">
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
