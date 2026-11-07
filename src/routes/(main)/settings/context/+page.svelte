<script lang="ts">
  import Label from '$lib/components/Label.svelte';
  import Setting from '$lib/components/Setting.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import { CONTEXT_TEXT_LIMIT } from '$lib/constants';
  import { m } from '$lib/paraglide/messages';
  import { smartContext, smartContextAutoInject, smartContextDisplay, smartContextLimit } from '$lib/stores.svelte';
  import SelectionBackgroundIcon from 'phosphor-svelte/lib/SelectionBackgroundIcon';
</script>

<div class="flex flex-col gap-2">
  <Setting icon={SelectionBackgroundIcon} title={m.context_settings()} tip={m.context_settings_hint()}>
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.context_enable_tip()} tipPlacement="duplex">{m.context_enable()}</Label>
      <Toggle bind:value={smartContext.current} />
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label inline tip={m.context_paragraph_limit_tip()} tipPlacement="duplex">
        {m.context_paragraph_limit()}
        <span class="ml-1 text-xs opacity-70">{smartContextLimit.current}</span>
      </Label>
      <input
        class="range max-w-2/5 text-emphasis range-xs"
        type="range"
        min={CONTEXT_TEXT_LIMIT.min}
        max={CONTEXT_TEXT_LIMIT.max}
        step={CONTEXT_TEXT_LIMIT.step}
        disabled={!smartContext.current}
        bind:value={smartContextLimit.current}
      />
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.context_auto_inject_tip()} tipPlacement="duplex">{m.context_auto_inject()}</Label>
      <Toggle bind:value={smartContextAutoInject.current} disabled={!smartContext.current} />
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.context_display_tip()} tipPlacement="duplex">{m.context_display()}</Label>
      <Toggle bind:value={smartContextDisplay.current} disabled={!smartContext.current} />
    </fieldset>
  </Setting>
</div>
