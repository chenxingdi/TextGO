<script lang="ts">
  import Label from '$lib/components/Label.svelte';
  import Select from '$lib/components/Select.svelte';
  import Setting from '$lib/components/Setting.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import { CONTEXT_CLIPBOARD_AGE, CONTEXT_TEXT_LIMIT } from '$lib/constants';
  import { m } from '$lib/paraglide/messages';
  import {
    contextClipboard,
    contextClipboardMaxAge,
    smartContext,
    smartContextDisplay,
    smartContextLimit
  } from '$lib/stores.svelte';
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
      <Label tip={m.context_clipboard_tip()} tipPlacement="duplex">{m.context_clipboard()}</Label>
      <Toggle bind:value={contextClipboard.current} disabled={!smartContext.current} />
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label inline tip={m.context_clipboard_age_tip()} tipPlacement="duplex">
        {m.context_clipboard_age()}
      </Label>
      <Select
        options={[
          { value: CONTEXT_CLIPBOARD_AGE.min, label: m.context_clipboard_age_10() },
          { value: CONTEXT_CLIPBOARD_AGE.default, label: m.context_clipboard_age_30() },
          { value: CONTEXT_CLIPBOARD_AGE.max, label: m.context_clipboard_age_60() }
        ]}
        bind:value={contextClipboardMaxAge.current}
        class="w-28 select-sm"
        disabled={!smartContext.current || !contextClipboard.current}
      />
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.context_display_tip()} tipPlacement="duplex">{m.context_display()}</Label>
      <Toggle bind:value={smartContextDisplay.current} disabled={!smartContext.current} />
    </fieldset>
  </Setting>
</div>
