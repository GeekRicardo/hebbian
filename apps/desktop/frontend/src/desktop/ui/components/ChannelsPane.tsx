import { WeChatPane } from "@/desktop/ui/components/WeChatPane";

/// 连接器设置页：把 hebbian 接到外部 IM。当前只有微信，未来在这里追加飞书等卡片。
export function ChannelsPane({ active }: { active: boolean }) {
  return (
    <div className="flex flex-col gap-6">
      <header>
        <h3 className="text-sm font-medium text-zinc-100">连接器</h3>
        <p className="mt-1 text-xs leading-relaxed text-zinc-400">
          把 hebbian 接到你常用的聊天工具，在手机上随时跟 AI 对话、远程审批。
        </p>
      </header>
      <WeChatPane active={active} />
    </div>
  );
}