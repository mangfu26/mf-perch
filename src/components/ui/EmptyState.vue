<script setup lang="ts">
/** 空状态占位：图标 + 说明 + 可选操作。 */
import type { Component } from "vue";

defineProps<{
  icon?: Component;
  title: string;
  hint?: string;
}>();
</script>

<template>
  <div
    class="flex h-full min-h-[240px] flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border-base px-6 py-12 text-center"
  >
    <div
      v-if="icon"
      class="grid h-12 w-12 place-items-center rounded-xl bg-surface text-text-muted"
    >
      <component :is="icon" class="h-5 w-5" />
    </div>
    <p class="text-[14px] font-medium">{{ title }}</p>
    <p v-if="hint" class="max-w-md text-[12.5px] leading-relaxed text-text-muted">{{ hint }}</p>
    <div v-if="$slots.default" class="mt-1"><slot /></div>
  </div>
</template>
