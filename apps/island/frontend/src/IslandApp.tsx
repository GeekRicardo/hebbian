import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import IslandCard from "./IslandCard";

interface CardData {
  id: string;
  cardType: string;
  title: string;
  body: string;
  sessionId?: string;
}

export default function IslandApp() {
  const [card, setCard] = useState<CardData | null>(null);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    document.body.style.background = "transparent";
    document.body.style.margin = "0";
    document.documentElement.style.background = "transparent";

    // 优先从 URL 参数读取 id，再 invoke 获取数据
    const params = new URLSearchParams(window.location.search);
    const id = params.get("id");

    if (id) {
      invoke<CardData | null>("island_get_card", { id }).then((data) => {
        if (data) setCard(data);
      });
    }

    // 也监听 push 事件（兼容 window.eval 推送方式）
    const handler = (e: Event) => {
      setCard((e as CustomEvent).detail);
    };
    window.addEventListener("island-init", handler);
    return () => window.removeEventListener("island-init", handler);
  }, []);

  if (!card) return null;

  return (
    <div
      style={{
        width: "100vw",
        height: "100vh",
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "flex-end",
        background: "transparent",
      }}
    >
      <IslandCard
        card={card}
        expanded={expanded}
        onToggle={() => setExpanded((e) => !e)}
        onAction={(action: string) => {
          invoke("island_action", { id: card.id, action }).catch(() => {});
          // 关闭窗口的 action 前先加离开动画
          if (action === "allow" || action === "deny" || action === "dismiss") {
            // 动画由 CSS .leaving 类控制，由 card 内部管理
          }
        }}
      />
    </div>
  );
}