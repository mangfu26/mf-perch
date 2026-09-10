<script setup lang="ts">
import AppSidebar from "@/components/layout/AppSidebar.vue";
</script>

<template>
  <div class="relative z-1 flex h-full w-full">
    <AppSidebar />
    <main class="flex-1 overflow-hidden">
      <RouterView v-slot="{ Component }">
        <component :is="Component" />
      </RouterView>
    </main>
  </div>
</template>
