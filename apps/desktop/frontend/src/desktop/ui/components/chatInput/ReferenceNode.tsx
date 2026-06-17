import { type JSX, useCallback } from "react";
import {
  $applyNodeReplacement,
  $getNodeByKey,
  type DOMExportOutput,
  DecoratorNode,
  type EditorConfig,
  type LexicalNode,
  type NodeKey,
  type SerializedLexicalNode,
  type Spread,
} from "lexical";
import { useLexicalNodeSelection } from "@lexical/react/useLexicalNodeSelection";
import { useLexicalComposerContext } from "@lexical/react/LexicalComposerContext";
import { MessageSquare, X } from "lucide-react";
import { cn, pathLeaf } from "@/desktop/ui/lib/utils";
import { PathTypeIcon } from "@/desktop/ui/components/workspaceFields";

/**
 * 引用 chip 的不可分割数据。`path` 是绝对路径（序列化与权限授权都用它），
 * `kind` 决定图标语义：文件路径 / 对话引用。
 */
export interface ReferencePayload {
  path: string;
  kind: "path" | "conversation";
}

export type SerializedReferenceNode = Spread<
  ReferencePayload,
  SerializedLexicalNode
>;

/**
 * 输入框里的引用高亮小块：渲染 `图标 + 末段名`，整块作为一个原子节点——
 * 光标跳过它、Backspace 整块删除。发送时由 serialize 还原成 `path` 纯文本。
 */
export class ReferenceNode extends DecoratorNode<JSX.Element> {
  __path: string;
  __kind: ReferencePayload["kind"];

  static getType(): string {
    return "reference";
  }

  static clone(node: ReferenceNode): ReferenceNode {
    return new ReferenceNode(
      { path: node.__path, kind: node.__kind },
      node.__key
    );
  }

  static importJSON(serialized: SerializedReferenceNode): ReferenceNode {
    return $createReferenceNode({
      path: serialized.path,
      kind: serialized.kind,
    });
  }

  constructor(payload: ReferencePayload, key?: NodeKey) {
    super(key);
    this.__path = payload.path;
    this.__kind = payload.kind;
  }

  exportJSON(): SerializedReferenceNode {
    return {
      ...super.exportJSON(),
      path: this.__path,
      kind: this.__kind,
    };
  }

  getPath(): string {
    return this.__path;
  }

  /** 发送时还原成纯文本：引用即路径原文。 */
  getTextContent(): string {
    return this.__path;
  }

  createDOM(): HTMLElement {
    const span = document.createElement("span");
    span.style.display = "inline-flex";
    span.style.verticalAlign = "middle";
    return span;
  }

  updateDOM(): false {
    return false;
  }

  exportDOM(): DOMExportOutput {
    const element = document.createElement("span");
    element.textContent = this.__path;
    return { element };
  }

  isInline(): true {
    return true;
  }

  decorate(): JSX.Element {
    return (
      <ReferenceChip nodeKey={this.__key} path={this.__path} kind={this.__kind} />
    );
  }
}

export function $createReferenceNode(payload: ReferencePayload): ReferenceNode {
  return $applyNodeReplacement(new ReferenceNode(payload));
}

export function $isReferenceNode(
  node: LexicalNode | null | undefined
): node is ReferenceNode {
  return node instanceof ReferenceNode;
}

function ReferenceChip({
  nodeKey,
  path,
  kind,
}: {
  nodeKey: NodeKey;
  path: string;
  kind: ReferencePayload["kind"];
}) {
  const [editor] = useLexicalComposerContext();
  const [isSelected] = useLexicalNodeSelection(nodeKey);

  const remove = useCallback(() => {
    editor.update(() => {
      $getNodeByKey(nodeKey)?.remove();
    });
  }, [editor, nodeKey]);

  return (
    <span
      className={cn(
        "mx-0.5 inline-flex max-w-[220px] items-center gap-1 rounded-md bg-primary/10 px-1.5 py-0.5 align-middle text-[12px] font-medium text-primary",
        isSelected && "ring-2 ring-primary/40"
      )}
      title={path}
      contentEditable={false}
    >
      {kind === "conversation" ? (
        <MessageSquare className="h-3 w-3 shrink-0" />
      ) : (
        <PathTypeIcon path={path} className="h-3 w-3 shrink-0" />
      )}
      <span className="truncate">{pathLeaf(path) || path}</span>
      <button
        type="button"
        onClick={remove}
        className="shrink-0 opacity-50 transition hover:opacity-100"
        aria-label="移除引用"
        tabIndex={-1}
      >
        <X className="h-3 w-3" />
      </button>
    </span>
  );
}
