<script setup lang="ts">
/**
 * 页面外壳：标题 + 操作区 + 内容区。
 * 统一各页面布局节奏，避免每个页面各自定义标题样式。
 */
import type { Component } from "vue";

defineProps<{
  title: string;
  subtitle?: string;
  icon?: Component;
}>();
</script>

<template>
  <div class="flex h-full flex-col overflow-hidden px-6 py-5">
    <header class="mb-4 flex shrink-0 items-start justify-between gap-4">
      <div class="flex items-start gap-3">
        <div
          v-if="icon"
          class="mt-0.5 grid h-9 w-9 shrink-0 place-items-center rounded-[10px] bg-accent-soft text-accent"
        >
          <component :is="icon" class="h-[18px] w-[18px]" />
        </div>
        <div>
          <h1 class="text-[19px] font-semibold tracking-tight">{{ title }}</h1>
          <p v-if="subtitle" class="mt-1 text-[12.5px] text-text-muted">{{ subtitle }}</p>
        </div>
      </div>
      <div class="flex shrink-0 items-center gap-2">
        <slot name="actions" />
      </div>
    </header>
    <div class="min-h-0 flex-1 overflow-auto">
      <slot />
    </div>
  </div>
</template>
