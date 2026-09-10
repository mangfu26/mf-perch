<script setup lang="ts">
/**
 * 密钥保护方式引导。
 *
 * 遵循 D6 / P1 / P2：
 * - 系统钥匙串可用时优先推荐（无感）
 * - 不可用时**必须让用户显式选择**，不得静默降级
 * - 选择本地密钥文件时明确标注安全性低于钥匙串
 */
import { ref, computed } from "vue";
import { useI18n } from "vue-i18n";
import { ShieldCheck, KeyRound, FileKey, AlertTriangle } from "lucide-vue-next";
import BaseModal from "@/components/ui/BaseModal.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import BaseInput from "@/components/ui/BaseInput.vue";
import FormField from "@/components/ui/FormField.vue";
import { useAppStore } from "@/stores/app";
import type { KeyProvider } from "@/lib/api";

const { t } = useI18n();
const app = useAppStore();

const props = defineProps<{
  open: boolean;
  /** 已初始化但需要解锁（K2），而非首次引导。 */
  unlockMode?: boolean;
}>();

const emit = defineEmits<{ (e: "close"): void }>();

const provider = ref<KeyProvider>("keyring");
const password = ref("");
const passwordConfirm = ref("");
const error = ref<string | null>(null);

const keyringAvailable = computed(
  () => app.keyStatus?.keyring_available ?? false,
);

/** 钥匙串不可用时默认落到主密码（安全强度不降低）。 */
const effectiveProvider = computed<KeyProvider>(() =>
  keyringAvailable.value ? provider.value : provider.value === "keyring" ? "master_password" : provider.value,
);

async function submit() {
  error.value = null;

  if (props.unlockMode) {
    if (!password.value) {
      error.value = "请输入主密码";
      return;
    }
    try {
      await app.unlock(password.value);
      password.value = "";
      emit("close");
    } catch {
      // 错误已由 store 提示。
    }
    return;
  }

  const p = effectiveProvider.value;

  if (p === "master_password") {
    if (password.value.length < 8) {
      error.value = "主密码至少 8 位";
      return;
    }
    if (password.value !== passwordConfirm.value) {
      error.value = "两次输入的主密码不一致";
      return;
    }
  }

  try {
    await app.initProvider(p, p === "master_password" ? password.value : undefined);
    password.value = "";
    passwordConfirm.value = "";
    emit("close");
  } catch {
    // 错误已由 store 提示。
  }
}
</script>

<template>
  <BaseModal
    :open="open"
    :title="unlockMode ? '解锁凭据' : '设置凭据保护方式'"
    :description="
      unlockMode
        ? '凭据由主密码保护，请输入主密码以解锁。'
        : 'SSH 密码与私钥将加密存储。请选择保护加密密钥的方式。'
    "
    width="md"
    @close="emit('close')"
  >
    <!-- 解锁模式 -->
    <template v-if="unlockMode">
      <FormField label="主密码" required>
        <BaseInput v-model="password" type="password" @keyup.enter="submit" />
      </FormField>
      <p
        v-if="error"
        class="mt-3 rounded-lg border border-danger/30 bg-danger-soft px-3 py-2 text-[12px] text-danger"
      >
        {{ error }}
      </p>
    </template>

    <!-- 首次引导 -->
    <template v-else>
      <div class="grid gap-2.5">
        <!-- 系统钥匙串：可用时推荐 -->
        <label
          v-if="keyringAvailable"
          class="flex cursor-pointer items-start gap-2.5 rounded-[10px] border px-3.5 py-3 transition-colors"
          :class="
            provider === 'keyring'
              ? 'border-accent bg-accent-soft'
              : 'border-border-base hover:bg-surface-hover'
          "
        >
          <input
            v-model="provider"
            type="radio"
            value="keyring"
            class="mt-0.5 accent-[var(--accent)]"
          />
          <span class="min-w-0">
            <span class="flex items-center gap-1.5 text-[13px] font-medium">
              <ShieldCheck class="h-4 w-4 text-success" />
              系统钥匙串（推荐）
            </span>
            <span class="mt-1 block text-[11.5px] leading-relaxed text-text-muted">
              密钥交由操作系统保管，无需输入密码，安全性最高。
            </span>
          </span>
        </label>

        <!-- 钥匙串不可用时明确告知（P1 / P2） -->
        <div
          v-else
          class="flex items-start gap-2 rounded-[10px] border border-warning/30 bg-warning-soft px-3.5 py-3"
        >
          <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0 text-warning" />
          <p class="text-[12px] leading-relaxed text-warning">
            当前系统未提供密钥服务（如 Linux 未安装 Secret Service），
            请在下方选择其他保护方式。
          </p>
        </div>

        <!-- 主密码 -->
        <label
          class="flex cursor-pointer items-start gap-2.5 rounded-[10px] border px-3.5 py-3 transition-colors"
          :class="
            effectiveProvider === 'master_password'
              ? 'border-accent bg-accent-soft'
              : 'border-border-base hover:bg-surface-hover'
          "
        >
          <input
            v-model="provider"
            type="radio"
            value="master_password"
            class="mt-0.5 accent-[var(--accent)]"
          />
          <span class="min-w-0">
            <span class="flex items-center gap-1.5 text-[13px] font-medium">
              <KeyRound class="h-4 w-4" />
              主密码
            </span>
            <span class="mt-1 block text-[11.5px] leading-relaxed text-text-muted">
              用主密码派生密钥（Argon2id）。安全强度不降低，且便于跨机器迁移；
              但每次启动需输入主密码。
            </span>
          </span>
        </label>

        <!-- 本地密钥文件 -->
        <label
          class="flex cursor-pointer items-start gap-2.5 rounded-[10px] border px-3.5 py-3 transition-colors"
          :class="
            effectiveProvider === 'local_file'
              ? 'border-accent bg-accent-soft'
              : 'border-border-base hover:bg-surface-hover'
          "
        >
          <input
            v-model="provider"
            type="radio"
            value="local_file"
            class="mt-0.5 accent-[var(--accent)]"
          />
          <span class="min-w-0">
            <span class="flex items-center gap-1.5 text-[13px] font-medium">
              <FileKey class="h-4 w-4" />
              本地密钥文件
            </span>
            <span class="mt-1 block text-[11.5px] leading-relaxed text-warning">
              密钥保存在本地文件（权限受限）。安全性低于系统钥匙串：
              若备份文件被他人获取，凭据可能被解密。
            </span>
          </span>
        </label>
      </div>

      <div v-if="effectiveProvider === 'master_password'" class="mt-4 grid gap-3">
        <FormField label="主密码" hint="至少 8 位；忘记后凭据无法恢复" required>
          <BaseInput v-model="password" type="password" />
        </FormField>
        <FormField label="确认主密码" required>
          <BaseInput v-model="passwordConfirm" type="password" @keyup.enter="submit" />
        </FormField>
      </div>

      <p
        v-if="error"
        class="mt-3 rounded-lg border border-danger/30 bg-danger-soft px-3 py-2 text-[12px] text-danger"
      >
        {{ error }}
      </p>
    </template>

    <template #footer>
      <BaseButton variant="default" :disabled="app.loading" @click="emit('close')">
        {{ t("common.cancel") }}
      </BaseButton>
      <BaseButton variant="primary" :disabled="app.loading" @click="submit">
        {{ app.loading ? t("common.loading") : unlockMode ? "解锁" : "确认" }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
