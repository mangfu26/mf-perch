<script setup lang="ts">
/**
 * 认证信息管理（D26）。
 *
 * 安全要求（D6 / Q10）：私钥正文与密码**不回显**，界面只展示指纹与格式；
 * 认证信息对 AI Agent 完全不可见。
 */
import { useI18n } from "vue-i18n";
import { KeyRound, Plus, Fingerprint } from "lucide-vue-next";
import PageShell from "@/components/layout/PageShell.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import StatusTag from "@/components/ui/StatusTag.vue";
import EmptyState from "@/components/ui/EmptyState.vue";

const { t } = useI18n();

// 阶段一：数据源尚未接入 Rust 侧。
const credentials: Array<{
  id: string;
  name: string;
  username: string;
  kind: "password" | "key";
  fingerprint: string | null;
  hasPassphrase: boolean;
  usedByHosts: string[];
}> = [];
</script>

<template>
  <PageShell :title="t('credential.title')" :icon="KeyRound">
    <template #actions>
      <BaseButton variant="primary">
        <Plus class="h-3.5 w-3.5" />
        {{ t("credential.add") }}
      </BaseButton>
    </template>

    <EmptyState
      v-if="credentials.length === 0"
      :icon="KeyRound"
      :title="t('credential.empty')"
      :hint="t('credential.emptyHint')"
    >
      <BaseButton variant="primary">
        <Plus class="h-3.5 w-3.5" />
        {{ t("credential.add") }}
      </BaseButton>
    </EmptyState>

    <div v-else class="grid gap-3">
      <article
        v-for="cred in credentials"
        :key="cred.id"
        class="rounded-xl border border-border-base bg-surface p-4 transition-colors hover:bg-surface-hover"
      >
        <div class="flex items-start justify-between gap-3">
          <div>
            <h3 class="text-[14px] font-semibold">
              {{ cred.name || cred.username }}
            </h3>
            <p class="mt-0.5 text-[12px] text-text-muted">
              {{ cred.username }}
            </p>
          </div>
          <StatusTag :tone="cred.kind === 'key' ? 'accent' : 'info'">
            {{ cred.kind === "key" ? t("credential.kindKey") : t("credential.kindPassword") }}
          </StatusTag>
        </div>

        <!-- 只展示指纹，绝不回显私钥正文（Q10） -->
        <div v-if="cred.fingerprint" class="mt-3 flex items-center gap-1.5 text-text-muted">
          <Fingerprint class="h-3.5 w-3.5" />
          <span class="font-mono text-[11.5px]">{{ cred.fingerprint }}</span>
        </div>

        <div class="mt-3 flex flex-wrap items-center gap-2">
          <StatusTag v-if="cred.hasPassphrase" tone="warning">
            {{ t("credential.passphrase") }}
          </StatusTag>
          <StatusTag tone="neutral">
            {{ t("credential.usedBy") }} {{ cred.usedByHosts.length }}
          </StatusTag>
        </div>
      </article>
    </div>
  </PageShell>
</template>
