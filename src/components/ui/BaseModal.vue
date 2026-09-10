<script setup lang="ts">
/**
 * 模态对话框。
 *
 * 用于表单与二次确认。危险操作（删除）必须二次确认（Q19 / D21）。
 */
import { watch, onUnmounted } from "vue";
import { X } from "lucide-vue-next";
import { cn } from "@/lib/utils";

const props = withDefaults(
  defineProps<{
    open: boolean;
    title: string;
    description?: string;
    width?: "sm" | "md" | "lg";
  }>(),
  { width: "md" },
);

const emit = defineEmits<{ (e: "close"): void }>();

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape" && props.open) emit("close");
}

watch(
  () => props.open,
  (open) => {
    if (open) window.addEventListener("keydown", onKeydown);
    else window.removeEventListener("keydown", onKeydown);
  },
);

onUnmounted(() => window.removeEventListener("keydown", onKeydown));

const widthClass = {
  sm: "max-w-md",
  md: "max-w-xl",
  lg: "max-w-3xl",
}[props.width];
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-start justify-center overflow-auto p-6 pt-[8vh]"
    >
      <!-- 遮罩：点击关闭 -->
      <div
        class="fixed inset-0 bg-black/50 backdrop-blur-[2px]"
        @click="emit('close')"
      />

      <div
        :class="
          cn(
            'relative z-10 w-full rounded-xl border border-border-base bg-bg-elevated shadow-2xl',
            widthClass,
          )
        "
        role="dialog"
        aria-modal="true"
      >
        <header
          class="flex items-start justify-between gap-4 border-b border-border-base px-5 py-4"
        >
          <div>
            <h2 class="text-[15px] font-semibold">{{ title }}</h2>
            <p
              v-if="description"
              class="mt-1 text-[12px] leading-relaxed text-text-muted"
            >
              {{ description }}
            </p>
          </div>
          <button
            type="button"
            class="rounded-lg p-1 text-text-muted transition-colors hover:bg-surface-hover hover:text-text-base"
            @click="emit('close')"
          >
            <X class="h-4 w-4" />
          </button>
        </header>

        <div class="px-5 py-4">
          <slot />
        </div>

        <footer
          v-if="$slots.footer"
          class="flex items-center justify-end gap-2 border-t border-border-base px-5 py-3.5"
        >
          <slot name="footer" />
        </footer>
      </div>
    </div>
  </Teleport>
</template>
