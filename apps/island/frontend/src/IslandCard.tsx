import { useEffect, useRef, useState } from "react";

/** 与 Rust protocol::NotificationCard 一致 */
interface CardData {
  id: string;
  cardType: string;
  title: string;
  body: string;
  sessionId?: string;
}

interface Props {
  card: CardData;
  expanded: boolean;
  onToggle: () => void;
  onAction: (action: string) => void;
}

const glyphContent: Record<string, string> = {
  info: "✓",
  approval: "⚡",
  question: "?",
};

export default function IslandCard({ card, expanded, onToggle, onAction }: Props) {
  const [leaving, setLeaving] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // info 卡片 3s 自动消失
  useEffect(() => {
    if (card.cardType !== "info" || leaving) return;
    const t = setTimeout(() => {
      setLeaving(true);
      setTimeout(() => onAction("dismiss"), 250);
    }, 3000);
    return () => clearTimeout(t);
  }, [card.cardType, leaving, onAction]);

  // 点击外部折叠
  useEffect(() => {
    if (!expanded) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onToggle();
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [expanded, onToggle]);

  const handleClick = () => {
    if (!expanded) {
      onToggle();
      return;
    }
    if (card.cardType === "question") {
      onAction("open");
    }
  };

  const className = [
    "island-card",
    expanded && "expanded",
    leaving && "leaving",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div ref={ref} className={className} onClick={handleClick}>
      {!expanded ? (
        <div className="island-card-compact">
          <div className={`glyph ${card.cardType}`}>
            {glyphContent[card.cardType] || "●"}
          </div>
          <div className="compact-title">{card.title}</div>
        </div>
      ) : (
        <div className="island-card-expanded">
          <div className="island-card-header">
            <div className={`glyph ${card.cardType}`}>
              {glyphContent[card.cardType] || "●"}
            </div>
            <div className="island-card-title">{card.title}</div>
          </div>
          {card.body && <div className="island-card-body">{card.body}</div>}
          {card.cardType === "approval" && (
            <div className="island-card-actions">
              <button
                className="btn-allow"
                onClick={(e) => {
                  e.stopPropagation();
                  onAction("allow");
                }}
              >
                允许
              </button>
              <button
                className="btn-deny"
                onClick={(e) => {
                  e.stopPropagation();
                  onAction("deny");
                }}
              >
                拒绝
              </button>
              <button
                className="btn-open"
                onClick={(e) => {
                  e.stopPropagation();
                  onAction("open");
                }}
              >
                打开
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}