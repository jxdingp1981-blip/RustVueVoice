<script setup lang="ts">
import { store } from "@/lib/store";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
</script>

<template>
  <Dialog v-model:open="store.helpOpen">
    <DialogContent class="sm:max-w-xl">
      <DialogHeader>
        <DialogTitle>使用说明</DialogTitle>
        <DialogDescription>RustVueVoice · Rulink/MR08 语音鼠标</DialogDescription>
      </DialogHeader>
      <div
        class="max-h-[52vh] overflow-y-auto rounded-lg border border-white/5 bg-white/[0.02] p-4 text-[13px] leading-relaxed whitespace-pre-wrap text-foreground/85"
      >{{ store.helpText }}</div>
    </DialogContent>
  </Dialog>
</template>
