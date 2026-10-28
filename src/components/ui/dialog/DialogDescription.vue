<script setup lang="ts">
import { DialogDescription, type DialogDescriptionProps } from "reka-ui";
import { cn } from "@/lib/utils";

const props = defineProps<DialogDescriptionProps & { class?: string }>();
</script>

<template>
  <DialogDescription data-slot="dialog-description" :class="cn('text-muted-foreground text-sm', props.class)">
    <slot />
  </DialogDescription>
</template>
