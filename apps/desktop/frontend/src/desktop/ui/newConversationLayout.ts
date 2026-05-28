export function shouldUseNewConversationInputLayout(params: {
  userMessageCount: number;
  isStreaming: boolean;
}): boolean {
  return params.userMessageCount === 0 && !params.isStreaming;
}
