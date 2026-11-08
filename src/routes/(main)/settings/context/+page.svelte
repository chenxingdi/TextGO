<script lang="ts">
  import { alert } from '$lib/components/Alert.svelte';
  import Button from '$lib/components/Button.svelte';
  import Label from '$lib/components/Label.svelte';
  import Select from '$lib/components/Select.svelte';
  import Setting from '$lib/components/Setting.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import { CONTEXT_TEXT_LIMIT, VISION_MARGIN } from '$lib/constants';
  import { m } from '$lib/paraglide/messages';
  import {
    anthropicApiKey,
    geminiApiKey,
    openaiApiKey,
    openrouterApiKey,
    providers,
    smartContext,
    smartContextDisplay,
    smartContextLimit,
    visionMarginBottom,
    visionMarginLeft,
    visionMarginRight,
    visionMarginTop,
    visionModel,
    visionPicking,
    visionProvider,
    visionRegion,
    xaiApiKey
  } from '$lib/stores.svelte';
  import ArrowClockwiseIcon from 'phosphor-svelte/lib/ArrowClockwiseIcon';
  import SelectionBackgroundIcon from 'phosphor-svelte/lib/SelectionBackgroundIcon';

  // model suggestions fetched from the configured provider; typing a model stays possible
  let visionModels: string[] = $state([]);
  let fetchingModels = $state(false);

  // provider choices come from the AI conversation settings, including custom providers
  const providerOptions = $derived([
    { value: '', label: m.context_vision_provider(), disabled: true },
    { value: 'ollama', label: 'Ollama' },
    { value: 'lmstudio', label: 'LM Studio' },
    { value: 'openrouter', label: 'OpenRouter', disabled: !openrouterApiKey.current },
    { value: 'openai', label: 'OpenAI', disabled: !openaiApiKey.current },
    { value: 'anthropic', label: 'Anthropic', disabled: !anthropicApiKey.current },
    { value: 'google', label: 'Google', disabled: !geminiApiKey.current },
    { value: 'xai', label: 'xAI', disabled: !xaiApiKey.current },
    ...providers.current.map((provider) => ({ value: provider.name, label: provider.name }))
  ]);

  // the vision feature only runs once both fields are filled and the switch is on
  const visionReady = $derived(!!visionProvider.current && !!visionModel.current);

  /**
   * Fetch the model list of the selected provider into the datalist.
   */
  async function loadModels() {
    if (!visionProvider.current || fetchingModels) {
      return;
    }
    fetchingModels = true;
    try {
      const { fetchVisionModels } = await import('$lib/vision');
      visionModels = await fetchVisionModels(visionProvider.current);
      if (visionModels.length === 0) {
        alert({ level: 'warning', message: m.context_vision_models_empty() });
      }
    } catch (error) {
      alert({
        level: 'error',
        message: m.context_vision_models_failed({ error: error instanceof Error ? error.message : String(error) })
      });
    } finally {
      fetchingModels = false;
    }
  }
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
      <Label tip={m.context_display_tip()} tipPlacement="duplex">{m.context_display()}</Label>
      <Toggle bind:value={smartContextDisplay.current} disabled={!smartContext.current} />
    </fieldset>
    <div class="divider my-0 opacity-60"></div>
    <!-- vision picking: the area around the selection is read by a vision model when no text is
         selectable, so it is configured next to the surrounding text capture -->
    <fieldset class="flex items-center justify-between gap-1">
      <Label tip={m.context_vision_enable_tip()} tipPlacement="duplex">{m.context_vision_enable()}</Label>
      <Toggle bind:value={visionPicking.current} disabled={!smartContext.current} />
    </fieldset>
    {#if smartContext.current && visionPicking.current}
      {#if !visionReady}
        <p class="px-1 pb-1 text-xs text-warning">{m.context_vision_missing()}</p>
      {/if}
      <fieldset class="flex items-center justify-between gap-1">
        <Label inline>{m.context_vision_provider()}</Label>
        <Select
          bind:value={visionProvider.current}
          options={providerOptions}
          class="max-w-2/5 select-sm"
          onchange={() => (visionModels = [])}
        />
      </fieldset>
      <fieldset class="flex items-center justify-between gap-1">
        <Label inline tip={m.context_vision_model_tip()}>{m.context_vision_model()}</Label>
        <span class="flex max-w-3/5 items-center gap-1">
          <input
            class="input min-w-0 grow shadow-sm input-sm"
            list="vision-model-options"
            spellcheck="false"
            placeholder="qwen2.5-vl"
            bind:value={visionModel.current}
          />
          <datalist id="vision-model-options">
            {#each visionModels as model (model)}
              <option value={model}></option>
            {/each}
          </datalist>
          <Button
            icon={ArrowClockwiseIcon}
            text={m.context_vision_fetch_models()}
            loading={fetchingModels}
            disabled={!visionProvider.current}
            onclick={loadModels}
          />
        </span>
      </fieldset>
      <fieldset class="flex items-center justify-between gap-1">
        <Label inline>{m.context_vision_region()}</Label>
        <Select
          value={visionRegion.current}
          options={[
            { value: 'selection', label: m.context_vision_region_selection() },
            { value: 'screen', label: m.context_vision_region_screen() }
          ]}
          class="max-w-2/5 select-sm"
          onchange={(event) => {
            visionRegion.current = event.currentTarget.value === 'screen' ? 'screen' : 'selection';
          }}
        />
      </fieldset>
      {#if visionRegion.current === 'selection'}
        <fieldset class="px-1">
          <Label tip={m.context_vision_margin_tip()}>{m.context_vision_margins()}</Label>
          <div class="grid grid-cols-2 gap-x-3 gap-y-1">
            <label class="flex items-center justify-between gap-2">
              <span class="text-sm opacity-80">{m.context_vision_margin_top()}</span>
              <input
                class="input w-20 shadow-sm input-sm"
                type="number"
                min={VISION_MARGIN.vertical.min}
                max={VISION_MARGIN.vertical.max}
                step={VISION_MARGIN.vertical.step}
                bind:value={visionMarginTop.current}
              />
            </label>
            <label class="flex items-center justify-between gap-2">
              <span class="text-sm opacity-80">{m.context_vision_margin_bottom()}</span>
              <input
                class="input w-20 shadow-sm input-sm"
                type="number"
                min={VISION_MARGIN.vertical.min}
                max={VISION_MARGIN.vertical.max}
                step={VISION_MARGIN.vertical.step}
                bind:value={visionMarginBottom.current}
              />
            </label>
            <label class="flex items-center justify-between gap-2">
              <span class="text-sm opacity-80">{m.context_vision_margin_left()}</span>
              <input
                class="input w-20 shadow-sm input-sm"
                type="number"
                min={VISION_MARGIN.horizontal.min}
                max={VISION_MARGIN.horizontal.max}
                step={VISION_MARGIN.horizontal.step}
                bind:value={visionMarginLeft.current}
              />
            </label>
            <label class="flex items-center justify-between gap-2">
              <span class="text-sm opacity-80">{m.context_vision_margin_right()}</span>
              <input
                class="input w-20 shadow-sm input-sm"
                type="number"
                min={VISION_MARGIN.horizontal.min}
                max={VISION_MARGIN.horizontal.max}
                step={VISION_MARGIN.horizontal.step}
                bind:value={visionMarginRight.current}
              />
            </label>
          </div>
        </fieldset>
      {/if}
    {/if}
  </Setting>
</div>
