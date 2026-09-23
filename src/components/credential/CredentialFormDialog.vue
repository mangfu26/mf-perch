<script setup lang="ts">
/**
 * 认证信息表单（新增 / 编辑）。
 *
 * 安全约束（Q10 / D6）：
 * - 私钥支持"文件导入"与"粘贴文本"（Q10 明确两者都要）
 * - 编辑时不回显已有密码/私钥正文，留空即保持不变
 * - 明确标注不支持 PuTTY 的 .ppk（Q10 排除）
 */
import { ref, watch, computed } from "vue";
import { useI18n } from "vue-i18n";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Upload, FileText } from "lucide-vue-next";
import BaseModal from "@/components/ui/BaseModal.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import BaseInput from "@/components/ui/BaseInput.vue";
import FormField from "@/components/ui/FormField.vue";
import type { CredentialInput, CredentialKind, CredentialSummary } from "@/lib/api";

const { t } = useI18n();

const props = defineProps<{
  open: boolean;
  credential?: CredentialSummary | null;
  saving?: boolean;
}>();

const emit = defineEmits<{
  (e: "submit", input: CredentialInput): void;
  (e: "close"): void;
}>();

const name = ref("");
const username = ref("");
const kind = ref<CredentialKind>("password");
const isPrivileged = ref(false);
const password = ref("");
const privateKey = ref("");
const passphrase = ref("");
const error = ref<string | null>(null);
const importing = ref(false);

watch(
  () => props.open,
  (open) => {
    if (!open) return;
    error.value = null;
    importing.value = false;

    const c = props.credential;
    name.value = c?.name ?? "";
    username.value = c?.username ?? "";
    kind.value = c?.kind ?? "password";
    // 特权身份标记必须回填：编辑时漏回填，用户"只改个名字"就会把它静默清掉（B5 同类）。
    isPrivileged.value = c?.is_privileged ?? false;
    // 正文与口令一律留空——后端从不返回它们，界面也不该回显。
    password.value = "";
    privateKey.value = "";
    passphrase.value = "";
  },
  { immediate: true },
);

const isEdit = computed(() => !!props.credential);
const title = computed(() =>
  isEdit.value ? t("credential.edit") : t("credential.add"),
);

/** 编辑已有凭据时，留空表示沿用原密钥（后端据此保留原值）。 */
const secretOptional = computed(() => isEdit.value);

async function importKeyFile() {
  importing.value = true;
  error.value = null;
  try {
    // 刻意**不**给 filters：OpenSSH 私钥按惯例没有扩展名（`~/.ssh/id_ed25519`），
    // 而原生选择器只列出当前过滤项匹配的文件——给了扩展名就等于把这些密钥挡在外面。
    // 格式判断交给内容：读到 PuTTY 的 .ppk 会明确报错，粘贴文本入口也始终可用。
    const selected = await openDialog({
      multiple: false,
      title: t("credential.privateKeyImport"),
    });

    if (typeof selected === "string") {
      const { readTextFile } = await import("@tauri-apps/plugin-fs");
      privateKey.value = await readTextFile(selected);
      if (privateKey.value.includes("PuTTY-User-Key-File")) {
        error.value =
          "检测到 PuTTY 的 .ppk 格式，暂不支持。请先用 PuTTYgen 转换为 OpenSSH 格式。";
        privateKey.value = "";
      }
    }
  } catch (e) {
    // 读取失败时给出明确原因，而不是静默失败（P1）。
    error.value = `读取私钥文件失败：${e instanceof Error ? e.message : String(e)}`;
  } finally {
    importing.value = false;
  }
}

function submit() {
  error.value = null;

  if (!username.value.trim()) {
    error.value = "请填写用户名";
    return;
  }

  const secret = kind.value === "password" ? password.value : privateKey.value;
  if (!secret && !secretOptional.value) {
    error.value =
      kind.value === "password" ? "请填写密码" : "请导入或粘贴私钥内容";
    return;
  }

  emit("submit", {
    id: props.credential?.id ?? null,
    name: name.value.trim() || null,
    username: username.value.trim(),
    kind: kind.value,
    is_privileged: isPrivileged.value,
    secret: secret || null,
    passphrase: passphrase.value || null,
  });
}
</script>

<template>
  <BaseModal :open="open" :title="title" @close="emit('close')">
    <div class="grid gap-4">
      <FormField :label="t('credential.name')" :hint="t('common.optional')">
        <BaseInput v-model="name" :placeholder="t('credential.namePlaceholder')" />
      </FormField>

      <FormField :label="t('credential.username')" required>
        <BaseInput v-model="username" mono />
      </FormField>

      <FormField :label="t('credential.kind')">
        <BaseInput v-model="kind" as="select">
          <option value="password">{{ t("credential.kindPassword") }}</option>
          <option value="key">{{ t("credential.kindKey") }}</option>
        </BaseInput>
      </FormField>

      <!-- 该身份是否登录即特权用户（D60）：标记落在认证信息上，
           所有引用它的主机一起生效，不需要逐台改。 -->
      <FormField :label="t('credential.isPrivileged')">
        <label
          class="flex cursor-pointer items-start gap-2.5 rounded-[9px] border px-3 py-2.5 transition-colors"
          :class="
            isPrivileged
              ? 'border-warning bg-warning-soft'
              : 'border-border-base hover:bg-surface-hover'
          "
        >
          <input
            v-model="isPrivileged"
            type="checkbox"
            class="mt-0.5 accent-[var(--accent)]"
          />
          <span class="text-[11.5px] leading-relaxed text-text-muted">
            {{ t("credential.isPrivilegedHint") }}
          </span>
        </label>
      </FormField>

      <!-- 密码认证 -->
      <FormField
        v-if="kind === 'password'"
        :label="t('credential.password')"
        :hint="secretOptional ? t('credential.secretUnchanged') : undefined"
        :required="!secretOptional"
      >
        <BaseInput v-model="password" type="password" />
      </FormField>

      <!-- 密钥认证 -->
      <template v-else>
        <FormField
          :label="t('credential.privateKey')"
          :hint="t('credential.privateKeyHint')"
          :required="!secretOptional"
        >
          <div class="flex flex-col gap-2">
            <div class="flex gap-2">
              <BaseButton
                size="sm"
                :disabled="importing"
                @click="importKeyFile"
              >
                <Upload class="h-3.5 w-3.5" />
                {{ importing ? t("common.loading") : t("credential.privateKeyImport") }}
              </BaseButton>
              <span class="flex items-center gap-1 text-[11.5px] text-text-muted">
                <FileText class="h-3.5 w-3.5" />
                {{ t("credential.privateKeyPaste") }}
              </span>
            </div>
            <BaseInput
              v-model="privateKey"
              as="textarea"
              :rows="6"
              mono
              :placeholder="secretOptional ? t('credential.secretUnchanged') : '-----BEGIN OPENSSH PRIVATE KEY-----'"
            />
          </div>
        </FormField>

        <FormField
          :label="t('credential.passphrase')"
          :hint="secretOptional ? t('credential.secretUnchanged') : t('credential.passphraseHint')"
        >
          <BaseInput v-model="passphrase" type="password" />
        </FormField>
      </template>

      <p
        v-if="error"
        class="rounded-lg border border-danger/30 bg-danger-soft px-3 py-2 text-[12px] leading-relaxed text-danger"
      >
        {{ error }}
      </p>

      <p class="text-[11.5px] leading-relaxed text-text-muted">
        {{ t("credential.fingerprintHint") }}
      </p>
    </div>

    <template #footer>
      <BaseButton variant="default" :disabled="saving" @click="emit('close')">
        {{ t("common.cancel") }}
      </BaseButton>
      <BaseButton variant="primary" :disabled="saving" @click="submit">
        {{ saving ? t("common.loading") : t("common.save") }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
