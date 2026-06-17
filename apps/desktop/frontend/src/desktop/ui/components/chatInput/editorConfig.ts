import type { InitialConfigType } from "@lexical/react/LexicalComposer";
import { ReferenceNode } from "./ReferenceNode";

/**
 * ChatInput 的 Lexical 配置。命名空间唯一；注册自定义 ReferenceNode；
 * 主题类名留空（chip 与文本样式各自在组件里写死，不走 Lexical theme）。
 */
export const chatInputEditorConfig: InitialConfigType = {
  namespace: "hebbian-chat-input",
  nodes: [ReferenceNode],
  onError(error) {
    // editor 内部异常不应静默吞掉，也不该崩溃整个输入框——交给上层日志。
    console.error("[ChatInput/Lexical]", error);
  },
  theme: {},
};
