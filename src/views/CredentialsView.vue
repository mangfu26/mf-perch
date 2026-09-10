<script setup lang="ts">
/**
 * 认证信息管理（D26）。
 *
 * 安全要求（D6 / Q10）：密码与私钥正文**永不回显**，
 * 界面只展示用户名、类型与指纹；认证信息对 AI Agent 完全不可见。
 */
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { KeyRound, Plus, Fingerprint, Pencil, Trash2, Link2 } from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import CredentialFormDialog from "@/components/credential/CredentialFormDialog.vue";
import { useCredentialsStore } from "@/stores/credentials";
import type { CredentialInput, CredentialSummary } from "@/lib/api";

const { t } = useI18n();
const store = useCredentialsStore();

const formOpen = ref(false);
const editing = ref<CredentialSummary | null>(null);
const saving = ref(false);

const confirmOpen = ref(false);
const deleting = ref<CredentialSummary | null>(null);
const removing = ref(false);

onMounted(() => store.refresh());

function openCreate() {
  editing.value = null;
  formOpen.value = true;
}

function openEdit(c: CredentialSummary) {
  editing.value = c;
  formOpen.value = true;
}

async function onSubmit(input: CredentialInput) {
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

function askDelete(c: CredentialSummary) {
  deleting.value = c;
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
</script>

<template>
  <PageShell :title="t('credential.title')" :icon="KeyRound">
    <template #actions>
      <BaseButton variant="primary" @click="openCreate">
        <Plus class="h-3.5 w-3.5" />
        {{ t("credential.add") }}
      </BaseButton>
    </template>

    <div
      v-if="store.loading && store.credentials.length === 0"
      class="text-[13px] text-text-muted"
    >
      {{ t("common.loading") }}
    </div>

    <EmptyState
      v-else-if="store.credentials.length === 0"
      :icon="KeyRound"
      :title="t('credential.empty')"
      :hint="t('credential.emptyHint')"
    >
      <BaseButton variant="primary" @click="openCreate">
        <Plus class="h-3.5 w-3.5" />
        {{ t("credential.add") }}
      </BaseButton>
    </EmptyState>

    <div v-else class="grid gap-3">
      <article
        v-for="cred in store.credentials"
        :key="cred.id"
        class="group rounded-xl border border-border-base bg-surface p-4 transition-colors hover:bg-surface-hover"
      >
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <h3 class="truncate text-[14px] font-semibold">
              {{ cred.name || cred.username }}
            </h3>
            <p class="mt-0.5 truncate font-mono text-[12px] text-text-muted">
              {{ cred.username }}
            </p>
          </div>

          <div class="flex items-center gap-1.5">
            <StatusTag :tone="cred.kind === 'key' ? 'accent' : 'info'">
              {{
                cred.kind === "key"
                  ? t("credential.kindKey")
                  : t("credential.kindPassword")
              }}
            </StatusTag>
            <div class="flex gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
              <BaseButton size="sm" variant="ghost" @click="openEdit(cred)">
                <Pencil class="h-3.5 w-3.5" />
              </BaseButton>
              <BaseButton size="sm" variant="ghost" @click="askDelete(cred)">
                <Trash2 class="h-3.5 w-3.5" />
              </BaseButton>
            </div>
          </div>
        </div>

        <!-- 只展示指纹，绝不回显私钥正文（Q10） -->
        <div
          v-if="cred.fingerprint"
          class="mt-3 flex items-center gap-1.5 text-text-muted"
        >
          <Fingerprint class="h-3.5 w-3.5 shrink-0" />
          <span class="truncate font-mono text-[11.5px]">{{ cred.fingerprint }}</span>
        </div>

        <div class="mt-3 flex flex-wrap items-center gap-2">
          <StatusTag v-if="cred.has_passphrase" tone="warning">
            {{ t("credential.passphrase") }}
          </StatusTag>
          <StatusTag v-if="cred.used_by_hosts.length > 0" tone="neutral">
            <Link2 class="h-3 w-3" />
            {{ t("credential.usedBy") }} {{ cred.used_by_hosts.length }}
          </StatusTag>
          <span
            v-if="cred.used_by_hosts.length > 0"
            class="truncate text-[11.5px] text-text-muted"
          >
            {{ cred.used_by_hosts.join("、") }}
          </span>
        </div>
      </article>
    </div>

    <CredentialFormDialog
      :open="formOpen"
      :credential="editing"
      :saving="saving"
      @submit="onSubmit"
      @close="formOpen = false"
    />

    <ConfirmDialog
      :open="confirmOpen"
      :title="t('credential.deleteConfirm')"
      :message="deleting ? (deleting.name || deleting.username) : ''"
      :warning="
        deleting && deleting.used_by_hosts.length > 0
          ? t('credential.inUseWarning', { count: deleting.used_by_hosts.length })
          : t('credential.deleteWarning')
      "
      :confirm-label="t('common.delete')"
      danger
      :loading="removing"
      @confirm="confirmDelete"
      @cancel="confirmOpen = false"
    />
  </PageShell>
</template>
