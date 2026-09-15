<script lang="ts">
  import Label from '$lib/components/Label.svelte';
  import Select from '$lib/components/Select.svelte';
  import Setting from '$lib/components/Setting.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import {
    POPUP_CORNER_RADIUS,
    POPUP_FONT_SIZE,
    POPUP_OPACITY,
    PROMPT_MARK,
    SCRIPT_MARK,
    TOOLBAR_ACTION_COUNT,
    TOOLBAR_ANCHOR_PERCENT,
    TOOLBAR_AUTO_HIDE_DELAY,
    TOOLBAR_CORNER_RADIUS,
    TOOLBAR_LINE_OFFSET,
    TOOLBAR_OPACITY,
    TOOLBAR_TEXT_GAP
  } from '$lib/constants';
  import { CONVERT_ACTIONS, GENERAL_ACTIONS, PROCESS_ACTIONS } from '$lib/executor';
  import { m } from '$lib/paraglide/messages';
  import {
    popupCornerRadius,
    popupFontSize,
    popupLineWrapping,
    popupOpacity,
    popupRememberPosition,
    prompts,
    scripts,
    toolbarAlwaysDisplayAction,
    toolbarAnchorPercent,
    toolbarAutoHide,
    toolbarAutoHideDelay,
    toolbarCornerRadius,
    toolbarLineOffset,
    toolbarMaxActions,
    toolbarOpacity,
    toolbarTextGap
  } from '$lib/stores.svelte';
  import type { Option } from '$lib/types';
  import AppWindowIcon from 'phosphor-svelte/lib/AppWindowIcon';
  import DeviceMobileSpeakerIcon from 'phosphor-svelte/lib/DeviceMobileSpeakerIcon';

  // generate toolbar action count options
  const toolbarActionCountOptions = Array.from(
    { length: TOOLBAR_ACTION_COUNT.max - TOOLBAR_ACTION_COUNT.min + 1 },
    (_, index) => {
      const value = TOOLBAR_ACTION_COUNT.min + index;
      return { value, label: `${value}` };
    }
  );

  // generate range marks
  const createRangeMarks = ({ min, max }: { min: number; max: number }, count: number) =>
    Array.from({ length: count }, (_, index) => Math.round(min + ((max - min) * index) / (count - 1)));

  const toolbarCornerRadiusMarks = createRangeMarks(TOOLBAR_CORNER_RADIUS, 4);
  const toolbarOpacityMarks = createRangeMarks(TOOLBAR_OPACITY, 3);
  const toolbarAutoHideDelayMarks = createRangeMarks(TOOLBAR_AUTO_HIDE_DELAY, 4);
  const toolbarTextGapMarks = createRangeMarks(TOOLBAR_TEXT_GAP, 3);
  const toolbarAnchorPercentMarks = createRangeMarks(TOOLBAR_ANCHOR_PERCENT, 3);
  const toolbarLineOffsetMarks = createRangeMarks(TOOLBAR_LINE_OFFSET, 3);

  const popupCornerRadiusMarks = createRangeMarks(POPUP_CORNER_RADIUS, 4);
  const popupOpacityMarks = createRangeMarks(POPUP_OPACITY, 3);
  const popupFontSizeMarks = createRangeMarks(POPUP_FONT_SIZE, 4);

  // actions that may keep the toolbar on screen, grouped by action type
  let alwaysDisplayOptions = $derived.by(() => {
    const options: Option[] = [{ value: '', label: m.toolbar_always_display_none() }];

    if (prompts.current.length > 0) {
      options.push({ value: '--prompt--', label: `-- ${m.ai()} --`, disabled: true });
      for (const prompt of prompts.current) {
        options.push({ value: PROMPT_MARK + prompt.id, label: prompt.id });
      }
    }

    if (scripts.current.length > 0) {
      options.push({ value: '--script--', label: `-- ${m.script()} --`, disabled: true });
      for (const script of scripts.current) {
        options.push({ value: SCRIPT_MARK + script.id, label: script.id });
      }
    }

    options.push({ value: '--general--', label: `-- ${m.general()} --`, disabled: true });
    options.push(...GENERAL_ACTIONS);
    options.push({ value: '--convert--', label: `-- ${m.text_case_convert()} --`, disabled: true });
    options.push(...CONVERT_ACTIONS);
    options.push({ value: '--process--', label: `-- ${m.text_processing()} --`, disabled: true });
    options.push(...PROCESS_ACTIONS);

    return options;
  });
</script>

<div class="flex flex-col gap-2">
  <Setting icon={DeviceMobileSpeakerIcon} iconClass="rotate-270" title={m.toolbar_settings()}>
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.max_action_count_explain()} tipPlacement="duplex">{m.max_action_count()}</Label>
      <Select options={toolbarActionCountOptions} bind:value={toolbarMaxActions.current} class="w-24 select-sm" />
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label class="min-w-0 flex-1" tip={m.toolbar_corner_radius_explain()} tipPlacement="duplex">
        {m.toolbar_corner_radius()}
      </Label>
      <label class="flex w-2/5 shrink-0 flex-col gap-2 pt-2">
        <input
          class="range w-full text-emphasis range-xs"
          type="range"
          min={TOOLBAR_CORNER_RADIUS.min}
          max={TOOLBAR_CORNER_RADIUS.max}
          step={TOOLBAR_CORNER_RADIUS.step}
          bind:value={toolbarCornerRadius.current}
        />
        <div class="flex justify-between text-xs opacity-70">
          {#each toolbarCornerRadiusMarks as radius (radius)}
            <span>{radius}px</span>
          {/each}
        </div>
      </label>
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label class="min-w-0 flex-1" tip={m.toolbar_opacity_explain()} tipPlacement="duplex">
        {m.toolbar_opacity()}
      </Label>
      <label class="flex w-2/5 shrink-0 flex-col gap-2 pt-2">
        <input
          class="range w-full text-emphasis range-xs"
          type="range"
          min={TOOLBAR_OPACITY.min}
          max={TOOLBAR_OPACITY.max}
          step={TOOLBAR_OPACITY.step}
          bind:value={toolbarOpacity.current}
        />
        <div class="flex justify-between text-xs opacity-70">
          {#each toolbarOpacityMarks as opacity (opacity)}
            <span>{opacity}%</span>
          {/each}
        </div>
      </label>
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.toolbar_auto_hide_explain()} tipPlacement="duplex">{m.toolbar_auto_hide()}</Label>
      <Toggle bind:value={toolbarAutoHide.current} />
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label class="min-w-0 flex-1">{m.toolbar_auto_hide_delay()}</Label>
      <label class="flex w-2/5 shrink-0 flex-col gap-2 pt-2" class:opacity-50={!toolbarAutoHide.current}>
        <input
          class="range w-full text-emphasis range-xs"
          type="range"
          min={TOOLBAR_AUTO_HIDE_DELAY.min}
          max={TOOLBAR_AUTO_HIDE_DELAY.max}
          step={TOOLBAR_AUTO_HIDE_DELAY.step}
          bind:value={toolbarAutoHideDelay.current}
          disabled={!toolbarAutoHide.current}
        />
        <div class="flex justify-between text-xs opacity-70">
          {#each toolbarAutoHideDelayMarks as delay (delay)}
            <span>{delay}s</span>
          {/each}
        </div>
      </label>
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label class="min-w-0 flex-1" tip={m.toolbar_text_gap_explain()} tipPlacement="duplex">
        {m.toolbar_text_gap()}
      </Label>
      <label class="flex w-2/5 shrink-0 flex-col gap-2 pt-2">
        <input
          class="range w-full text-emphasis range-xs"
          type="range"
          min={TOOLBAR_TEXT_GAP.min}
          max={TOOLBAR_TEXT_GAP.max}
          step={TOOLBAR_TEXT_GAP.step}
          bind:value={toolbarTextGap.current}
        />
        <div class="flex justify-between text-xs opacity-70">
          {#each toolbarTextGapMarks as gap (gap)}
            <span>{gap}px</span>
          {/each}
        </div>
      </label>
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label class="min-w-0 flex-1" tip={m.toolbar_anchor_percent_explain()} tipPlacement="duplex">
        {m.toolbar_anchor_percent()}
      </Label>
      <label class="flex w-2/5 shrink-0 flex-col gap-2 pt-2">
        <input
          class="range w-full text-emphasis range-xs"
          type="range"
          min={TOOLBAR_ANCHOR_PERCENT.min}
          max={TOOLBAR_ANCHOR_PERCENT.max}
          step={TOOLBAR_ANCHOR_PERCENT.step}
          bind:value={toolbarAnchorPercent.current}
        />
        <div class="flex justify-between text-xs opacity-70">
          {#each toolbarAnchorPercentMarks as percent (percent)}
            <span>{percent}%</span>
          {/each}
        </div>
      </label>
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label class="min-w-0 flex-1" tip={m.toolbar_line_offset_explain()} tipPlacement="duplex">
        {m.toolbar_line_offset()}
      </Label>
      <label class="flex w-2/5 shrink-0 flex-col gap-2 pt-2">
        <input
          class="range w-full text-emphasis range-xs"
          type="range"
          min={TOOLBAR_LINE_OFFSET.min}
          max={TOOLBAR_LINE_OFFSET.max}
          step={TOOLBAR_LINE_OFFSET.step}
          bind:value={toolbarLineOffset.current}
        />
        <div class="flex justify-between text-xs opacity-70">
          {#each toolbarLineOffsetMarks as offset (offset)}
            <span>{offset}px</span>
          {/each}
        </div>
      </label>
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.toolbar_always_display_explain()} tipPlacement="duplex">{m.toolbar_always_display()}</Label>
      <Select
        options={alwaysDisplayOptions}
        bind:value={toolbarAlwaysDisplayAction.current}
        class="max-w-3/5 select-sm"
      />
    </fieldset>
  </Setting>
  <Setting icon={AppWindowIcon} title={m.popup_settings()}>
    <fieldset class="flex items-center justify-between gap-1">
      <Label class="min-w-0 flex-1" tip={m.popup_corner_radius_explain()} tipPlacement="duplex">
        {m.popup_corner_radius()}
      </Label>
      <label class="flex w-2/5 shrink-0 flex-col gap-2 pt-2">
        <input
          class="range w-full text-emphasis range-xs"
          type="range"
          min={POPUP_CORNER_RADIUS.min}
          max={POPUP_CORNER_RADIUS.max}
          step={POPUP_CORNER_RADIUS.step}
          bind:value={popupCornerRadius.current}
        />
        <div class="flex justify-between text-xs opacity-70">
          {#each popupCornerRadiusMarks as radius (radius)}
            <span>{radius}px</span>
          {/each}
        </div>
      </label>
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label class="min-w-0 flex-1" tip={m.popup_opacity_explain()} tipPlacement="duplex">
        {m.popup_opacity()}
      </Label>
      <label class="flex w-2/5 shrink-0 flex-col gap-2 pt-2">
        <input
          class="range w-full text-emphasis range-xs"
          type="range"
          min={POPUP_OPACITY.min}
          max={POPUP_OPACITY.max}
          step={POPUP_OPACITY.step}
          bind:value={popupOpacity.current}
        />
        <div class="flex justify-between text-xs opacity-70">
          {#each popupOpacityMarks as opacity (opacity)}
            <span>{opacity}%</span>
          {/each}
        </div>
      </label>
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label class="min-w-0 flex-1" tip={m.popup_font_size_explain()} tipPlacement="duplex">
        {m.popup_font_size()}
      </Label>
      <label class="flex w-2/5 shrink-0 flex-col gap-2 pt-2">
        <input
          class="range w-full text-emphasis range-xs"
          type="range"
          min={POPUP_FONT_SIZE.min}
          max={POPUP_FONT_SIZE.max}
          step={POPUP_FONT_SIZE.step}
          bind:value={popupFontSize.current}
        />
        <div class="flex justify-between text-xs opacity-70">
          {#each popupFontSizeMarks as size (size)}
            <span>{size}px</span>
          {/each}
        </div>
      </label>
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.popup_remember_position_explain()} tipPlacement="duplex">{m.popup_remember_position()}</Label>
      <Toggle bind:value={popupRememberPosition.current} />
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.popup_line_wrapping_explain()} tipPlacement="duplex">{m.popup_line_wrapping()}</Label>
      <Toggle bind:value={popupLineWrapping.current} />
    </fieldset>
  </Setting>
</div>
