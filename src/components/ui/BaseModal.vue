<script setup lang="ts">
/**
 * 模态对话框。
 *
 * 用于表单与二次确认。危险操作（删除）必须二次确认（Q19 / D21）。
 *
 * ## 高度与滚动规范（theme-spec.md「弹窗」）
 *
 * **只有内容区滚动**：标题与底部按钮常驻，永不随内容滚出视口。
 *
 * 早期实现把 `overflow-auto` 放在外层容器上，于是窗口偏矮时**整个面板**
 * （含标题与按钮）一起滚动——用户既看不到自己在填什么表单，
 * 也要滚到底才能点"保存"。五个弹窗共用本组件，因此这里是唯一的修复点。
 *
 * 关键三点（缺一不可）：
 * - 面板 `flex flex-col` + `max-h`：永不超出视口；
 * - 内容区 `min-h-0 flex-1 overflow-y-auto`：flex 子项默认不收缩，
 *   少了 `min-h-0` 内容区不会滚，而是继续把面板撑高；
 * - 头/脚 `shrink-0`：不被内容挤压。
 *
 * 对齐保持**顶部对齐 + 距顶 8vh**（不垂直居中）：认证信息在"密码 ↔ 私钥"
 * 之间切换时高度会变，居中会让整个框上下移动，顶部对齐视觉更稳。
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
    <!--
      外层**不滚动**（只做定位与遮罩）：
      滚动交给面板内部的内容区，标题与按钮因此始终可见。
    -->
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-start justify-center overflow-hidden p-6 pt-[8vh]"
    >
      <!-- 遮罩：点击关闭 -->
      <div
        class="fixed inset-0 bg-black/50 backdrop-blur-[2px]"
        @click="emit('close')"
      />

      <div
        :class="
          cn(
            // max-h 预留：顶部 8vh + 底部 p-6（1.5rem），保证面板永不超出视口。
            'relative z-10 flex max-h-[calc(100vh-8vh-1.5rem)] w-full flex-col rounded-xl border border-border-base bg-bg-elevated shadow-2xl',
            widthClass,
          )
        "
        role="dialog"
        aria-modal="true"
      >
        <header
          class="flex shrink-0 items-start justify-between gap-4 border-b border-border-base px-5 py-4"
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

        <!--
          内容区：**唯一**的滚动容器。
          `min-h-0` 必不可少（flex 子项默认不收缩）；
          `overscroll-contain` 防止滚到边界后带动背后页面；
          `scrollbar-gutter:stable` 避免滚动条出现/消失时内容横向跳动
          （切换"密码/私钥"时会反复触发）。
        -->
        <div
          class="min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 py-4 [scrollbar-gutter:stable]"
        >
          <slot />
        </div>

        <footer
          v-if="$slots.footer"
          class="flex shrink-0 items-center justify-end gap-2 border-t border-border-base px-5 py-3.5"
        >
          <slot name="footer" />
        </footer>
      </div>
    </div>
  </Teleport>
</template>
