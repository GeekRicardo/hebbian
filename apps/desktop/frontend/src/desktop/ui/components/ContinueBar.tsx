import { useStore, selectCurrentSessionStream } from "@/desktop/ui/store/useStore";
import { InputSuggestions } from "./InputSuggestions";

/**
 * 输入框上方的「继续」入口（架构 §4.3 / §7.3）。
 * 复用建议 chip 的样式——报错文案已经在 toast 区展示，这里只放一个轻量入口。
 * 数据来自 `currentSession.pending_continue`（落盘可恢复，重启后仍可见）；
 * 上一轮正常完成后 agent_loop 清空它，本组件自然消失。
 *
 * 点击行为由全局设置 `continue_strategy` 决定：
 * - `send_continue`（默认）：发一条 user「继续」消息再跑。末尾天然是 user，
 *   彻底避免 assistant prefill 400。
 * - `resume_loop`：不加任何消息，原样再起一次 agent_loop（失败重发 / 截断续写）。
 * - `manual`：只把光标聚焦输入框，让用户改 prompt 再发。
 */
export function ContinueBar({
  onSend,
  onFocusInput,
}: {
  onSend: (text: string) => void;
  onFocusInput?: () => void;
}) {
  const pending = useStore((s) => s.currentSession?.pending_continue);
  const isStreaming = useStore((s) => !!selectCurrentSessionStream(s).streamingMessageId);
  const isSuspended = useStore((s) => selectCurrentSessionStream(s).suspended !== null);
  const strategy = useStore(
    (s) => s.appSettings?.general.continue_strategy ?? "send_continue",
  );
  const sendUserMessage = useStore((s) => s.sendUserMessage);

  // 正在跑或已挂起都别显示——Continue 只属于上一轮真正结束后的续作入口。
  if (!pending || isStreaming || isSuspended) return null;

  const handleClick = () => {
    // 乐观清掉续作入口，让 chip 立刻消失；这一轮若再次异常，agent_loop 会重新写入。
    const cur = useStore.getState().currentSession;
    if (cur) {
      useStore.setState({ currentSession: { ...cur, pending_continue: null } });
    }
    if (strategy === "manual") {
      onFocusInput?.();
      return;
    }
    if (strategy === "resume_loop") {
      // 原样再起 agent_loop，不追加任何消息（末尾若是 assistant 靠重建层补 user）。
      void sendUserMessage("", [], null, { skipOptimisticUser: true, continueRun: true });
      return;
    }
    // send_continue（默认）：发一条真实「继续」user message——末尾天然是 user。
    onSend("继续");
  };

  return (
    <InputSuggestions
      suggestions={[{ label: "Continue", value: "continue" }]}
      onSelect={handleClick}
    />
  );
}
