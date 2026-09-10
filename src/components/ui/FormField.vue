<script setup lang="ts">
/** 表单字段容器：标签 + 说明 + 内容，统一表单排版节奏。 */
defineProps<{
  label: string;
  hint?: string;
  required?: boolean;
}>();
</script>

<template>
  <label class="block">
    <span class="mb-1.5 flex items-center gap-1 text-[12.5px] font-medium">
      {{ label }}
      <span v-if="required" class="text-danger">*</span>
    </span>
    <slot />
    <span v-if="hint" class="mt-1 block text-[11.5px] leading-relaxed text-text-muted">
      {{ hint }}
    </span>
  </label>
</template>
