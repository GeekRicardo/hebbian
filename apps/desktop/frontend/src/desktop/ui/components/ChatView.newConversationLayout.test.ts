import { shouldUseNewConversationInputLayout } from "../newConversationLayout";

function assertEqual(actual: boolean, expected: boolean, label: string) {
  if (actual !== expected) {
    throw new Error(`${label}: expected ${expected}, got ${actual}`);
  }
}

assertEqual(
  shouldUseNewConversationInputLayout({
    userMessageCount: 0,
    isStreaming: false,
  }),
  true,
  "keeps the new conversation layout after model-only assistant metadata changes"
);

assertEqual(
  shouldUseNewConversationInputLayout({
    userMessageCount: 1,
    isStreaming: false,
  }),
  false,
  "docks the input only after the user starts the conversation"
);

assertEqual(
  shouldUseNewConversationInputLayout({
    userMessageCount: 0,
    isStreaming: true,
  }),
  false,
  "docks the input while a run is streaming"
);
