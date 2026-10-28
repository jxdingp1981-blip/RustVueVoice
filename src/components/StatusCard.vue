<script setup lang="ts">
import type { Component } from "vue";
import { cn } from "@/lib/utils";

const props = withDefaults(
  defineProps<{
    label: string;
    value: string;
    icon: Component;
    tone?: "ok" | "err" | "neutral";
    busy?: boolean;
  }>(),
  { tone: "neutral", busy: false },
);

const toneClass = {
  ok: "text-emerald-300",
  err: "text-rose-300",
  neutral: "text-foreground/90",
}[props.tone];

const dotClass = {
  ok: "bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.8)]",
  err: "bg-rose-400 shadow-[0_0_8px_rgba(251,113,133,0.8)]",
  neutral: "bg-slate-500",
}[props.tone];
</script>

<template>
  <div
    :class="
      cn(
        'group relative overflow-hidden rounded-xl border border-white/5 bg-white/[0.03] px-3.5 py-3 transition-colors hover:border-white/10 hover:bg-white/[0.05]',
      )
    "
  >
    <div class="flex items-center gap-2 text-[11px] font-medium tracking-wide text-muted-foreground">
      <component :is="icon" class="size-3.5 opacity-70" />
      <span>{{ label }}</span>
    </div>
    <div class="mt-2 flex items-center gap-2">
      <span
        v-if="!busy"
        class="inline-block size-1.5 rounded-full"
        :class="dotClass"
      />
      <span v-if="busy" class="inline-block size-1.5 animate-pulse rounded-full bg-amber-400" />
      <span :class="cn('text-sm font-semibold leading-none', toneClass)">{{ value }}</span>
    </div>
  </div>
</template>
