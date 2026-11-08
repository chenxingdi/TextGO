import type { ChatMessage } from '$lib/llm';
import { createLLMClient, providerConnection } from '$lib/llm';
import { visionModel, visionPicking, visionProvider } from '$lib/stores.svelte';
import { invoke } from '@tauri-apps/api/core';
import { fetch } from '@tauri-apps/plugin-http';

/**
 * Fixed instruction for screenshot transcription.
 *
 * Temperature zero and the `NO_TEXT` marker are the two supports of this prompt: the first keeps
 * the model from inventing text, the second gives the caller an explicit "nothing readable" answer.
 */
const TRANSCRIBE_PROMPT = [
  'You are an OCR engine. Transcribe every visible character in the image.',
  'Rules:',
  '- Output only the text, line by line, in the original order.',
  '- Keep the original language, punctuation and spacing. Never translate.',
  '- Never correct, complete or rephrase anything, even if it looks like a typo.',
  '- Ignore UI chrome: icons, buttons, borders, cursors, timestamps that are visually separated from the text.',
  '- If there is no readable text, output exactly: NO_TEXT',
  '- Do not add quotes, markdown, labels or explanations.'
].join('\n');

/** Marker the model returns when the image carries no readable text. */
const NO_TEXT = 'no_text';

/**
 * Remove wrapping the model added around a transcription.
 *
 * @param raw - model response
 * @returns plain transcribed text, or an empty string when nothing readable was found
 */
export function cleanTranscription(raw: string): string {
  let text = raw.trim();
  if (!text) {
    return '';
  }

  // strip a fenced block the model wrapped the answer in
  const fenced = text.match(/^```[^\n]*\n([\s\S]*?)\n?```$/);
  if (fenced?.[1] !== undefined) {
    text = fenced[1].trim();
  }

  // strip one pair of wrapping quotes
  if (
    (text.startsWith('"') && text.endsWith('"')) ||
    (text.startsWith('“') && text.endsWith('”')) ||
    (text.startsWith('「') && text.endsWith('」'))
  ) {
    text = text.slice(1, -1).trim();
  }

  if (text.toLowerCase().replace(/[\s_-]+/g, '') === NO_TEXT) {
    return '';
  }

  // drop a leading label such as "Text:" or "识别结果："
  text = text.replace(/^(?:text|transcription|ocr|识别结果|文字|转写结果)\s*[:：]\s*/i, '');

  // the model sometimes inserts spaces between CJK characters it considers separate words
  text = text.replace(
    /([\u3400-\u9fff\u3000-\u303f\uff00-\uffef])\s+(?=[\u3400-\u9fff\u3000-\u303f\uff00-\uffef])/g,
    '$1'
  );

  return text.trim();
}

/**
 * Whether vision picking is configured well enough to read context.
 *
 * @returns whether the switch is on and both the provider and the model are filled
 */
export function visionReady(): boolean {
  return visionPicking.current && !!visionProvider.current && !!visionModel.current;
}

/**
 * Read context text out of a screenshot of the current selection.
 *
 * This is the last context acquisition step: the caller already asked the application for the
 * surrounding text and got nothing, so the backend captures the configured area while the source
 * application still owns focus. Any failure is reported as an empty result, which simply leaves
 * the action without context.
 *
 * @returns transcribed text, or an empty string when vision picking is off or produced nothing
 */
export async function transcribeContext(): Promise<string> {
  // the settings live in the shared store; a selection can arrive before the first read finishes
  await Promise.all([visionPicking.ready, visionProvider.ready, visionModel.ready]).catch(() => {});
  if (!visionReady()) {
    return '';
  }

  try {
    const image = await invoke<string | null>('capture_selection_image');
    if (!image) {
      return '';
    }

    const client = createLLMClient(visionProvider.current);
    const messages = [
      { role: 'system', content: TRANSCRIBE_PROMPT },
      {
        role: 'user',
        content: [
          { type: 'text', text: 'Transcribe the text in this image.' },
          { type: 'image_url', image_url: { url: `data:image/png;base64,${image}`, detail: 'high' } }
        ]
      }
    ] as ChatMessage[];

    let content = '';
    for await (const chunk of client.chat(
      {
        model: visionModel.current,
        messages: messages,
        max_tokens: 512,
        temperature: 0
      },
      // the request body sets stream: true before the custom parameters are merged
      { stream: false }
    )) {
      content += chunk.content;
    }

    const text = cleanTranscription(content);
    if (!text) {
      console.debug('Vision picking found no readable context');
    }
    return text;
  } catch (error) {
    console.error(`Failed to transcribe the context image: ${error}`);
    return '';
  }
}

/**
 * List the models the configured provider offers.
 *
 * Only used to fill the model suggestions; typing a model name by hand stays possible because
 * some providers do not expose a complete list.
 *
 * @param provider - LLM provider type or custom provider name
 * @returns model identifiers in the order the provider returned them
 */
export async function fetchVisionModels(provider: string): Promise<string[]> {
  const connection = providerConnection(provider);
  if (!connection) {
    throw new Error('The selected provider is not configured');
  }

  const headers: Record<string, string> = { Origin: 'http://localhost' };
  let url = `${connection.baseUrl}/models`;
  if (provider === 'anthropic') {
    headers['x-api-key'] = connection.apiKey;
    headers['anthropic-version'] = '2023-06-01';
  } else if (provider === 'google') {
    url = `https://generativelanguage.googleapis.com/v1beta/models?key=${encodeURIComponent(connection.apiKey)}`;
  } else {
    headers.Authorization = `Bearer ${connection.apiKey}`;
  }

  const response = await fetch(url, { method: 'GET', headers });
  if (!response.ok) {
    throw new Error(`${response.status} ${response.statusText}`.trim());
  }

  const body = (await response.json()) as {
    data?: { id?: string }[];
    models?: { name?: string }[];
  };
  const names =
    provider === 'google'
      ? (body.models ?? []).map((model) => (model.name ?? '').replace(/^models\//, ''))
      : (body.data ?? []).map((model) => model.id ?? '');

  return [...new Set(names.filter((name) => name.length > 0))].sort((a, b) => a.localeCompare(b));
}
