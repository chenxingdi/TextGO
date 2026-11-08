import type { LLMProvider } from '$lib/types';
import type { LLMClient } from './base';

import {
  anthropicApiKey,
  geminiApiKey,
  lmstudioHost,
  ollamaHost,
  openaiApiKey,
  openrouterApiKey,
  providers,
  xaiApiKey
} from '$lib/stores.svelte';
import { AnthropicClient } from './anthropic';
import { OpenAICompatibleClient } from './base';
import { GeminiClient } from './google';
import { LMStudioClient } from './lmstudio';
import { OllamaClient } from './ollama';
import { OpenAIClient } from './openai';
import { OpenRouterClient } from './openrouter';
import { XAIClient } from './xai';

/**
 * Custom LLM client for user-defined providers.
 */
class CustomLLMClient extends OpenAICompatibleClient {
  constructor(baseUrl: string, apiKey: string) {
    super(baseUrl, apiKey);
  }
}

/** Endpoint and credential used to reach a provider. */
export type ProviderConnection = {
  /** OpenAI compatible base URL, without a trailing slash. */
  baseUrl: string;
  /** Bearer token or provider specific API key. */
  apiKey: string;
};

/**
 * Resolve the endpoint and credential of a provider.
 *
 * Model listing needs the same connection as the chat client, but without constructing a client
 * that requires a configured key.
 *
 * @param provider - LLM provider type or custom provider name
 * @returns connection details, or `null` when the provider is unknown
 */
export function providerConnection(provider: LLMProvider | string): ProviderConnection | null {
  switch (provider) {
    case 'ollama':
      return { baseUrl: `${ollamaHost.current || 'http://127.0.0.1:11434'}/v1`, apiKey: 'ollama' };
    case 'lmstudio':
      return { baseUrl: `${lmstudioHost.current || 'http://127.0.0.1:1234'}/v1`, apiKey: 'lm-studio' };
    case 'openrouter':
      return openrouterApiKey.current
        ? { baseUrl: 'https://openrouter.ai/api/v1', apiKey: openrouterApiKey.current }
        : null;
    case 'openai':
      return openaiApiKey.current ? { baseUrl: 'https://api.openai.com/v1', apiKey: openaiApiKey.current } : null;
    case 'anthropic':
      return anthropicApiKey.current
        ? { baseUrl: 'https://api.anthropic.com/v1', apiKey: anthropicApiKey.current }
        : null;
    case 'google':
      return geminiApiKey.current
        ? { baseUrl: 'https://generativelanguage.googleapis.com/v1beta/openai', apiKey: geminiApiKey.current }
        : null;
    case 'xai':
      return xaiApiKey.current ? { baseUrl: 'https://api.x.ai/v1', apiKey: xaiApiKey.current } : null;
    default: {
      const custom = providers.current.find((p) => p.name === provider);
      return custom ? { baseUrl: custom.baseUrl, apiKey: custom.apiKey } : null;
    }
  }
}

/**
 * Create LLM client based on provider type.
 *
 * @param provider - LLM provider type
 * @returns LLM client instance
 */
export function createLLMClient(provider: LLMProvider | string): LLMClient {
  switch (provider) {
    case 'ollama':
      return new OllamaClient();
    case 'lmstudio':
      return new LMStudioClient();
    case 'openrouter':
      return new OpenRouterClient();
    case 'openai':
      return new OpenAIClient();
    case 'anthropic':
      return new AnthropicClient();
    case 'google':
      return new GeminiClient();
    case 'xai':
      return new XAIClient();
    default: {
      // check if it's a custom provider
      const customProvider = providers.current.find((p) => p.name === provider);
      if (customProvider) {
        return new CustomLLMClient(customProvider.baseUrl, customProvider.apiKey);
      }
      throw new Error(`Unsupported LLM provider: ${provider}`);
    }
  }
}

// export types for external usage
export type { ChatCompletionMessageParam as ChatMessage } from 'openai/resources/chat/completions';
export type { LLMResponseChunk } from './base';
export type { LLMClient, LLMProvider };
