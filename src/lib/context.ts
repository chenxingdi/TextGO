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
 * Check whether a captured context carries information beyond the application name.
 *
 * The application alone adds little to a translation request, so it does not justify a context
 * block or a chip on its own.
 *
 * @param context - captured context
 * @returns whether the context contains a window title, URL or surrounding text
 */
export function hasContext(context?: SelectionContext | null): boolean {
  return [context?.title?.trim(), context?.url?.trim(), context?.paragraph?.trim()].some(Boolean);
}

/**
 * Build the system-prompt block that gives the model background about the selection.
 *
 * The block is only produced when there is something to inject, so requests without context
 * keep their original prompt byte for byte.
 *
 * @param context - captured context
 * @returns Markdown section, or an empty string when the context carries no information
 */
export function composeContextBlock(context?: SelectionContext | null): string {
  if (!hasContext(context)) {
    return '';
  }

  const facts: string[] = [];
  const title = context?.title?.trim();
  const url = context?.url?.trim();
  const app = appName(context?.appId);
  if (title) {
    facts.push(`- Window title: ${title}`);
  }
  if (app) {
    facts.push(`- Source application: ${app}`);
  }
  if (url) {
    facts.push(`- Page URL: ${url}`);
  }

  const paragraph = (context?.paragraph?.trim() ?? '').slice(0, CONTEXT_PARAGRAPH_LIMIT);
  const sections = [
    '## Context',
    [
      'Background information for interpreting the text to translate.',
      'Use it only to pick the right meaning, terminology and tone.',
      'Never translate, quote or describe this background in the response.'
    ].join(' ')
  ];
  if (facts.length > 0) {
    sections.push(facts.join('\n'));
  }
  if (paragraph) {
    sections.push(`Text around the selection:\n"""\n${paragraph}\n"""`);
  }
  return sections.join('\n\n');
}
