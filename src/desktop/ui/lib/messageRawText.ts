type RawMessagePart =
  | {
      type: "text";
      text: string;
    }
  | {
      type: string;
    };

type RawMessage = {
  role: string;
  content: string;
  parts?: RawMessagePart[];
};

export function getMessageRawText(message: RawMessage): string {
  const textParts = message.parts?.filter(
    (part): part is Extract<RawMessagePart, { type: "text" }> =>
      part.type === "text"
  );

  if (textParts?.length) {
    return textParts.map((part) => part.text).join("");
  }

  return message.content;
}

export function canShowRawMessage(message: RawMessage): boolean {
  if (message.role !== "user" && message.role !== "assistant") {
    return false;
  }

  return getMessageRawText(message).length > 0;
}
