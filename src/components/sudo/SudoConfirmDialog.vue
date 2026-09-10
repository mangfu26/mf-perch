<script setup lang="ts">
/**
 * sudo 提权确认框（Q33 ask 模式）。
 *
 * 设计考虑：
 * - 这是**人类掌控提权**的入口，因此信息要够判断：哪台主机、哪个终端
 * - 两个按钮同等显眼，不做"默认允许"的诱导
 * - 明确告知超时后果，让用户知道不响应等于拒绝
 * - 显示剩余时间，避免用户以为可以慢慢决定
 */
import { computed, onUnmounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { ShieldAlert, Clock } from "lucide-vue-next";
import BaseModal from "@/components/ui/BaseModal.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import { useSudoStore } from "@/stores/sudo";

const { t } = useI18n();
const sudo = useSudoStore();

/** 与后端 SUDO_ASK_TIMEOUT_SECS 一致。 */
const TIMEOUT_SECONDS = 60;

const remaining = ref(TIMEOUT_SECONDS);
let timer: ReturnType<typeof setInterval> | null = null;

const request = computed(() => sudo.current());

watch(
  request,
  (req) => {
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
    if (!req) return;

    remaining.value = TIMEOUT_SECONDS;
    timer = setInterval(() => {
      remaining.value -= 1;
      // 归零时不再更新；后端会超时并让 sudo 失败，队列由 respond 清理。
      if (remaining.value <= 0 && timer) {
        clearInterval(timer);
        timer = null;
      }
    }, 1000);
  },
  { immediate: true },
);

onUnmounted(() => {
  if (timer) clearInterval(timer);
});

async function decide(allow: boolean) {
  const req = request.value;
  if (!req) return;
  await sudo.respond(req, allow);
}

/** 剩余时间占比，用于进度条。 */
const progress = computed(() =>
  Math.max(0, Math.min(100, (remaining.value / TIMEOUT_SECONDS) * 100)),
);
</script>

<template>
  <BaseModal
    :open="!!request"
    :title="t('sudo.title')"
    width="sm"
    @close="decide(false)"
  >
    <div v-if="request" class="grid gap-3.5">
      <div class="flex items-start gap-2.5">
        <div
          class="mt-0.5 grid h-9 w-9 shrink-0 place-items-center rounded-[10px] bg-warning-soft text-warning"
        >
          <ShieldAlert class="h-[18px] w-[18px]" />
        </div>
        <p class="text-[13px] leading-relaxed">
          {{ t("sudo.message", { host: request.host_label }) }}
        </p>
      </div>

      <!-- 终端 ID：便于人类确认是哪个会话在请求提权 -->
      <div class="rounded-[9px] border border-border-base bg-surface px-3 py-2.5">
        <p class="text-[11.5px] text-text-muted">{{ t("terminal.id") }}</p>
        <p class="mt-0.5 break-all font-mono text-[11.5px]">
          {{ request.terminal_id }}
        </p>
      </div>

      <!-- 剩余时间：让用户知道不响应即等于拒绝 -->
      <div>
        <div class="mb-1.5 flex items-center justify-between text-[11.5px] text-text-muted">
          <span class="flex items-center gap-1">
            <Clock class="h-3 w-3" />
            {{ t("sudo.timeoutHint") }}
          </span>
          <span class="font-mono">{{ Math.max(0, remaining) }}s</span>
        </div>
        <div class="h-1 overflow-hidden rounded-full bg-surface-hover">
          <div
            class="h-full rounded-full bg-warning transition-all duration-1000 ease-linear"
            :style="{ width: `${progress}%` }"
          />
        </div>
      </div>

      <p class="text-[11.5px] leading-relaxed text-text-muted">
        {{ t("sudo.denyHint") }}
      </p>
    </div>

    <template #footer>
      <BaseButton
        variant="default"
        :disabled="sudo.responding"
        @click="decide(false)"
      >
        {{ t("sudo.deny") }}
      </BaseButton>
      <BaseButton
        variant="primary"
        :disabled="sudo.responding"
        @click="decide(true)"
      >
        {{ t("sudo.allow") }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
