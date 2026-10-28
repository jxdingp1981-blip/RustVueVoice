<script setup lang="ts">
import { DialogTitle, type DialogTitleProps } from "reka-ui";
import { cn } from "@/lib/utils";

const props = defineProps<DialogTitleProps & { class?: string }>();
</script>

<template>
  <DialogTitle data-slot="dialog-title" :class="cn('text-lg leading-none font-semibold', props.class)">
    <slot />
  </DialogTitle>
</template>
