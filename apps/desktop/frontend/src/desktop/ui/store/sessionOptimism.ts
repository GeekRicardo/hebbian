import type { MessageAttachment, MessageMeta, Session } from "../types";

interface OptimisticUserMessageOptions {
  id: string;
  now: number;
  meta?: MessageMeta | null;
}

export function appendOptimisticUserMessage(
  session: Session,
  content: string,
  attachments: MessageAttachment[],
  options: OptimisticUserMessageOptions
): Session {
  return {
    ...session,
    updated_at: options.now,
    messages: [
      ...session.messages,
      {
        id: options.id,
        role: "user",
        content,
        attachments,
        created_at: options.now,
        meta: options.meta ?? null,
      },
    ],
  };
}
