import type { SelectionContext } from '$lib/types';

/**
 * Maximum number of surrounding-text characters kept in a context block.
 *
 * The native side already clips the captured text, so this is only a safety net for
 * unexpected payloads.
 */
export const CONTEXT_PARAGRAPH_LIMIT = 4000;

/**
 * Derive the application name from a platform identifier.
 *
 * @param appId - executable path on Windows or bundle identifier on macOS
 * @returns bare application name, or an empty string when the identifier is missing
 */
export function appName(appId?: string): string {
  const value = appId?.trim();
  if (!value) {
    return '';
  }
  // executable paths end with the file name; bundle identifiers end with the application name
  const name = value.replace(/\.exe$/i, '').split(/[\\/]/).at(-1) ?? '';
  return name.includes('.') ? (name.split('.').at(-1) ?? name) : name;
}

/**
 * Read the surrounding text out of a captured context.
 *
 * @param context - captured context
 * @returns trimmed surrounding text, or an empty string when none was captured
 */
export function contextParagraph(context?: SelectionContext | null): string {
  return context?.paragraph?.trim() ?? '';
}

/**
 * Build the system-prompt block that gives the model background about the selection.
 *
 * Only the surrounding text is injected. Window titles, page URLs and application names are left
 * out on purpose: they rarely help disambiguate a word and they leak what the user is looking at.
 * The block is only produced when there is text to inject, so requests without context keep their
 * original prompt byte for byte.
 *
 * @param context - captured context
 * @returns Markdown section, or an empty string when the context carries no surrounding text
 */
export function composeContextBlock(context?: SelectionContext | null): string {
  const paragraph = contextParagraph(context).slice(0, CONTEXT_PARAGRAPH_LIMIT);
  if (!paragraph) {
    return '';
  }

  return [
    '## Context',
    [
      'Text around the selection, provided as background for interpreting it.',
      'Use it only to pick the right meaning, terminology and tone.',
      'Never translate, quote or describe this background in the response.'
    ].join(' '),
    `Text around the selection:\n"""\n${paragraph}\n"""`
  ].join('\n\n');
}
