<script setup lang="ts">
import { computed } from "vue";
import { Battery } from "lucide-vue-next";

const props = defineProps<{ pct: number | null; size?: number }>();

const R = 15.5;
const C = 2 * Math.PI * R;

const color = computed(() => {
  if (props.pct == null) return "var(--muted-foreground)";
  return props.pct >= 20 ? "#34d399" : "#fb7185";
});

const dash = computed(() => {
  if (props.pct == null) return `${C} ${C}`;
  const p = Math.max(0, Math.min(100, props.pct));
  return `${(p / 100) * C} ${C}`;
});

const label = computed(() => (props.pct == null ? "—" : `${props.pct}%`));
</script>

<template>
  <div class="flex items-center gap-2">
    <div class="relative" :style="{ width: (size ?? 38) + 'px', height: (size ?? 38) + 'px' }">
      <svg viewBox="0 0 38 38" class="size-full -rotate-90">
        <circle cx="19" cy="19" :r="R" fill="none" stroke="rgba(255,255,255,0.08)" stroke-width="3" />
        <circle
          v-if="pct != null"
          cx="19"
          cy="19"
          :r="R"
          fill="none"
          :stroke="color"
          stroke-width="3"
          stroke-linecap="round"
          :stroke-dasharray="dash"
          class="transition-[stroke-dasharray] duration-500"
        />
      </svg>
      <span
        class="absolute inset-0 flex items-center justify-center text-[9.5px] font-bold tabular-nums"
        :style="{ color }"
      >
        {{ label }}
      </span>
    </div>
    <div class="hidden flex-col leading-tight sm:flex">
      <span class="flex items-center gap-1 text-[11px] font-medium text-muted-foreground">
        <Battery class="size-3" />
        鼠标电量
      </span>
      <span class="text-[10px] text-muted-foreground/60">
        {{ pct == null ? "等待设备应答" : "实时刷新" }}
      </span>
    </div>
  </div>
</template>
