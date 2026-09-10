<script setup lang="ts">
/**
 * 二次确认对话框。
 *
 * 危险操作（删除终端、删除主机、删除凭据）必须经此确认（Q19 / D21）。
 * 支持 `danger` 样式与不可恢复的警告文案。
 */
import BaseModal from "./BaseModal.vue";
import BaseButton from "./BaseButton.vue";
import { AlertTriangle } from "lucide-vue-next";

withDefaults(
  defineProps<{
    open: boolean;
    title: string;
    message: string;
    /** 不可恢复的后果说明，用警示色展示。 */
    warning?: string;
    confirmLabel?: string;
    danger?: boolean;
    loading?: boolean;
  }>(),
  {
    confirmLabel: "确认",
    danger: false,
    loading: false,
  },
);

const emit = defineEmits<{
  (e: "confirm"): void;
  (e: "cancel"): void;
}>();
</script>

<template>
  <BaseModal :open="open" :title="title" width="sm" @close="emit('cancel')">
    <p class="text-[13px] leading-relaxed">{{ message }}</p>
    <div
      v-if="warning"
      class="mt-3 flex items-start gap-2 rounded-lg border border-danger/30 bg-danger-soft px-3 py-2.5"
    >
      <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0 text-danger" />
      <p class="text-[12px] leading-relaxed text-danger">{{ warning }}</p>
    </div>

    <template #footer>
      <BaseButton variant="default" :disabled="loading" @click="emit('cancel')">
        取消
      </BaseButton>
      <BaseButton
        :variant="danger ? 'danger' : 'primary'"
        :disabled="loading"
        @click="emit('confirm')"
      >
        {{ loading ? "处理中…" : confirmLabel }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
