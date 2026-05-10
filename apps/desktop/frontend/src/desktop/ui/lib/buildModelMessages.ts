import type { Message, MessageAttachment, Session } from "@/desktop/ui/types";

type TextBlock = { type: "text"; text: string };
type ImageBlock = {
  type: "image_url";
  image_url: { url: string };
};
type UserContent = string | Array<TextBlock | ImageBlock>;

interface OpenAiToolCall {
  id: string;
  type: "function";
  function: { name: string; arguments: string };
}

type ModelMessage =
  | { role: "system"; content: string }
  | { role: "user"; content: UserContent }
  | {
      role: "assistant";
      content: string;
      reasoning?: string;
      tool_calls?: OpenAiToolCall[];
    }
  | { role: "tool"; tool_call_id: string; content: string };

function userContentFromMessage(m: Message): UserContent {
  const attachments = m.attachments ?? [];
  if (attachments.length === 0) return m.content;
  const blocks: Array<TextBlock | ImageBlock> = [];
  if (m.content) blocks.push({ type: "text", text: m.content });
  for (const a of attachments) {
    if (a.kind === "image") {
      blocks.push({
        type: "image_url",
        image_url: { url: `data:${a.media_type};base64,${a.data}` },
      });
    } else {
      blocks.push(textFileBlock(a));
    }
  }
  return blocks;
}

function textFileBlock(a: Extract<MessageAttachment, { kind: "text_file" }>): TextBlock {
  return {
    type: "text",
    text: `<file name="${a.name}" media_type="${a.media_type}">\n${a.content}\n</file>`,
  };
}

function argumentsString(input: unknown, raw?: string): string {
  if (raw && raw.length > 0) return raw;
  if (input === undefined) return "{}";
  try {
    return JSON.stringify(input);
  } catch {
    return String(input);
  }
}

function pushAssistant(out: ModelMessage[], m: Message) {
  const textParts: string[] = [];
  const reasoningParts: string[] = [];
  const toolCalls: OpenAiToolCall[] = [];
  const toolResults: ModelMessage[] = [];

  if (m.parts?.length) {
    for (const p of m.parts) {
      if (p.type === "text") {
        textParts.push(p.text);
      } else if (p.type === "reasoning") {
        reasoningParts.push(p.text);
      } else {
        toolCalls.push({
          id: p.id,
          type: "function",
          function: {
            name: p.name,
            arguments: argumentsString(p.input, p.arguments),
          },
        });
        if (p.result != null) {
          toolResults.push({
            role: "tool",
            tool_call_id: p.id,
            content: p.result,
          });
        }
      }
    }
  } else if (m.tool_calls?.length) {
    if (m.content) textParts.push(m.content);
    for (const tc of m.tool_calls) {
      toolCalls.push({
        id: tc.id,
        type: "function",
        function: { name: tc.name, arguments: argumentsString(tc.input) },
      });
      if (tc.result != null) {
        toolResults.push({
          role: "tool",
          tool_call_id: tc.id,
          content: tc.result,
        });
      }
    }
  } else if (m.content) {
    textParts.push(m.content);
  }

  const assistant: Extract<ModelMessage, { role: "assistant" }> = {
    role: "assistant",
    content: textParts.join(""),
  };
  if (reasoningParts.length > 0) assistant.reasoning = reasoningParts.join("");
  if (toolCalls.length > 0) assistant.tool_calls = toolCalls;
  out.push(assistant);
  out.push(...toolResults);
}

/**
 * 把 session.messages 翻译成 OpenAI 风格的 messages 数组,
 * 用于「显示原始 JSON」面板,让用户直观看到发给模型的载荷形态。
 *
 * - 始终在最前面注入 system prompt(若 session 有配置)
 * - 跳过 marker 消息
 * - 截断到 uptoMessageId(包含),让每条 bubble 都能看到自己出现时的上下文
 */
export function buildModelMessages(
  session: Session,
  uptoMessageId?: string
): ModelMessage[] {
  const out: ModelMessage[] = [];
  if (session.system_prompt) {
    out.push({ role: "system", content: session.system_prompt });
  }
  for (const m of session.messages) {
    if (m.role !== "marker") {
      if (m.role === "user") {
        out.push({ role: "user", content: userContentFromMessage(m) });
      } else if (m.role === "assistant") {
        pushAssistant(out, m);
      }
    }
    if (uptoMessageId && m.id === uptoMessageId) break;
  }
  return out;
}
