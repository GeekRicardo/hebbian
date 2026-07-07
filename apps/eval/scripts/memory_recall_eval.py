#!/usr/bin/env python3
"""离线评测 Hebbian 项目记忆抽取与联想注入。

默认不读写真实 memory 目录，只读取 session.jsonl，并把评测产物写到 target/memory-eval/。
目标：用较老 Hebbian 项目会话构建记忆，用较新会话做 holdout，模拟每个 user run 的记忆注入并输出评审材料。
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
from collections import Counter, defaultdict
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

HEBBIAN_WORKDIR = "/Users/ricardo/code/ricardo/rust/hebbian"
DEFAULT_SESSIONS_DIR = Path.home() / ".hebbian" / "sessions"
DEFAULT_OUT = Path("target/memory-eval")

STOPWORDS = {
    "好的", "老大", "现在", "然后", "这个", "那个", "一个", "一些", "进行", "因为", "所以", "如果",
    "需要", "不要", "不是", "可以", "已经", "就是", "还是", "比较", "看看", "一下", "里面", "出来",
    "the", "and", "for", "with", "from", "this", "that", "into", "then", "true", "false", "null",
}

STRONG_DOMAIN_TERMS = {
    "memory", "recall", "terminal", "sidebar", "scroll", "compaction", "codex", "mimicode",
    "automode", "terminalsurface", "editorpane", "记忆", "联想", "抽取", "注入", "终端", "滚动", "上下文",
    "压缩", "侧边栏", "评测", "深睡", "建边", "openclaw", "hermes", "iterm", "xterm", "toolcall",
}

WEAK_DOMAIN_TERMS = {
    "session", "run", "turn", "tool", "bash", "context", "desktop", "hebweb", "cli", "agent", "goal",
    "permission", "judge", "changelog", "架构", "会话", "ui", "bug", "architecture", "verification",
}

DOMAIN_TERMS = STRONG_DOMAIN_TERMS | WEAK_DOMAIN_TERMS
GENERIC_TAGS = {"ui", "context", "cli", "architecture", "bug", "verification", "project"}
SPECIFIC_TAGS = {
    "memory", "recall", "eval", "terminal", "xterm", "iterm", "scroll", "chat-view", "sidebar", "compaction",
    "tool-output", "codex", "mimicode", "automode", "permission", "model-io", "openclaw", "hermes",
    "session-state", "storage", "resume", "goal", "hooks", "provider", "image", "file-tree",
}

TOPIC_KEYWORDS = {
    "scroll": {"scroll", "滚动", "可视", "viewport", "usermessage", "最底部", "插队", "向上", "下箭头", "上箭头"},
    "terminal": {"terminal", "终端", "iterm", "iterm2", "xterm", "terminalsurface", "shell", "ansi", "pty"},
    "memory": {"memory", "记忆", "recall", "联想", "抽取", "注入", "openclaw", "hermes", "评测", "深睡", "建边"},
    "compaction": {"codex", "mimicode", "上下文", "context", "compaction", "压缩", "toolcall", "tool output", "truncate", "truncated", "elide"},
}

TOPIC_TAGS = {
    "scroll": {"scroll", "chat-view"},
    "terminal": {"terminal", "xterm", "iterm"},
    "memory": {"memory", "recall", "eval", "openclaw", "hermes"},
    "compaction": {"compaction", "tool-output", "codex", "mimicode"},
}

TOPIC_REQUIRED_TERMS = {
    "scroll": {"scroll", "滚动", "viewport", "可视", "最底部", "上箭头", "下箭头", "usermessage", "插队"},
    "terminal": {"terminal", "终端", "iterm", "iterm2", "xterm", "ansi", "pty"},
    "memory": {"memory", "记忆", "recall", "联想", "召回", "抽取", "注入", "评测", "openclaw", "hermes"},
    "compaction": {"codex", "mimicode", "上下文", "context", "compaction", "压缩", "toolcall", "tool output", "truncate", "truncated", "elide"},
}

SYSTEM_NOTIFICATION_PREFIX = "[SYSTEM NOTIFICATION - NOT USER INPUT]"

FILE_RE = re.compile(r"[\w./-]+\.(?:rs|tsx|ts|py|md|json|toml|css|html|mjs|yaml|yml)")
SYMBOL_RE = re.compile(r"`([^`]{2,80})`")
ASCII_RE = re.compile(r"[A-Za-z][A-Za-z0-9_/-]{1,}")
CJK_RE = re.compile(r"[\u4e00-\u9fff]+")


@dataclass
class Msg:
    id: str
    role: str
    content: str
    created_at: int = 0


@dataclass
class Sess:
    id: str
    title: str
    created_at: int
    updated_at: int
    workdir: str | None
    messages: list[Msg]
    path: str


@dataclass
class Memory:
    id: str
    summary: str
    content: str
    source_session: str
    source_title: str
    kind: str
    tags: list[str]
    anchors: list[str]
    tokens: list[str]
    strength: float = 1.0
    learned_round: int = 0


@dataclass
class RecallHit:
    memory_id: str
    score: float
    direct: float
    spread: float
    reasons: list[str] = field(default_factory=list)


def read_sessions(root: Path, workdir: str) -> list[Sess]:
    sessions: list[Sess] = []
    for p in root.glob("*/session.jsonl"):
        try:
            lines = p.read_text(encoding="utf-8", errors="ignore").splitlines()
            if not lines:
                continue
            meta_line = json.loads(lines[0])
            if meta_line.get("type") != "meta" or meta_line.get("workdir") != workdir:
                continue
            msgs: list[Msg] = []
            for line in lines[1:]:
                try:
                    obj = json.loads(line)
                except Exception:
                    continue
                if obj.get("type") == "message" and obj.get("role") in {"user", "assistant"}:
                    content = (obj.get("content") or "").strip()
                    if content:
                        msgs.append(Msg(obj.get("id", ""), obj.get("role", ""), content, obj.get("created_at") or 0))
            if not msgs:
                continue
            sessions.append(Sess(
                id=meta_line.get("id") or p.parent.name,
                title=meta_line.get("title") or "",
                created_at=meta_line.get("created_at") or 0,
                updated_at=meta_line.get("updated_at") or meta_line.get("created_at") or 0,
                workdir=meta_line.get("workdir"),
                messages=msgs,
                path=str(p),
            ))
        except Exception:
            continue
    sessions.sort(key=lambda s: s.updated_at, reverse=True)
    return sessions


def cjk_bigrams(text: str) -> list[str]:
    out: list[str] = []
    for run in CJK_RE.findall(text):
        if len(run) == 1:
            out.append(run)
        else:
            out.extend(run[i:i + 2] for i in range(len(run) - 1))
    return out


def tokenize(text: str) -> list[str]:
    toks: list[str] = []
    for t in ASCII_RE.findall(text):
        t = t.lower().strip("_-./")
        if len(t) >= 2 and t not in STOPWORDS:
            toks.append(t)
    toks.extend(t for t in cjk_bigrams(text) if t not in STOPWORDS)
    return toks


def anchors(text: str) -> list[str]:
    vals: list[str] = []
    vals.extend(FILE_RE.findall(text))
    for raw in SYMBOL_RE.findall(text):
        raw = raw.strip()
        if len(raw) >= 2 and len(raw.split()) <= 4:
            vals.append(raw)
    for t in ASCII_RE.findall(text):
        if any(ch.isupper() for ch in t[1:]) or "_" in t or "/" in t:
            vals.append(t)
    seen = []
    for v in vals:
        v = v.strip()
        if v and v not in seen:
            seen.append(v)
    return seen[:16]


def normalize_title(title: str) -> str:
    title = re.sub(r"^(修复|新增|调整|完善|研究|实现|重构)", "", title).strip()
    return title or "未命名主题"


def infer_tags(text: str, title: str) -> list[str]:
    low = (text + " " + title).lower()
    pairs = [
        ("memory", ["记忆", "memory", "长期记忆"]),
        ("recall", ["recall", "联想", "召回", "注入"]),
        ("eval", ["评测", "holdout", "baseline", "judge", "noisy", "thin"]),
        ("openclaw", ["openclaw"]),
        ("hermes", ["hermes"]),
        ("compaction", ["compaction", "压缩", "compact"]),
        ("tool-output", ["tool output", "toolcall", "tool call", "truncate", "truncated", "elide", "head", "tail"]),
        ("codex", ["codex"]),
        ("mimicode", ["mimicode"]),
        ("terminal", ["终端", "terminal", "shell", "ansi", "pty"]),
        ("iterm", ["iterm", "iterm2"]),
        ("xterm", ["xterm"]),
        ("scroll", ["滚动", "scroll", "viewport", "最底部", "上箭头", "下箭头"]),
        ("chat-view", ["usermessage", "插队", "可视区域", "输入框下方", "消息列表"]),
        ("sidebar", ["sidebar", "侧边栏", "右侧"]),
        ("file-tree", ["filetree", "文件树", "软链"]),
        ("session-state", ["session.json", "会话状态", "重连", "恢复", "读取到哪里"]),
        ("storage", ["storage", "落盘", "文件锁", "atomic", "持久化"]),
        ("automode", ["automode"]),
        ("permission", ["permission", "approval", "审批"]),
        ("hooks", ["hook", "hooks", "goal"]),
        ("provider", ["provider", "deepseek", "模型", "model_io", "model-io"]),
        ("image", ["图片", "image", "paste", "粘贴"]),
        ("context", ["上下文", "context"]),
        ("ui", ["前端", "组件", "tsx", "ui", "monaco", "样式"]),
        ("cli", ["cli", "heb ", "daemon", "ndjson"]),
        ("architecture", ["架构", "设计", "协议", "surface", "agent-core"]),
        ("bug", ["bug", "修复", "报错", "失败", "根因", "不刷新", "卡住"]),
        ("verification", ["验证", "测试", "cargo", "pnpm", "tsc", "build"]),
    ]
    tags = [tag for tag, keys in pairs if any(k in low for k in keys)]
    specific = [t for t in tags if t in SPECIFIC_TAGS]
    generic = [t for t in tags if t in GENERIC_TAGS]
    return (specific + generic)[:7] or ["project"]


def informative_sentence(text: str) -> str:
    cleaned = re.sub(r"\s+", " ", text).strip()
    pieces = re.split(r"(?<=[。！？.!?])\s+|\n+", text)
    cues = ["根因", "关键", "实现", "验证", "注意", "必须", "应该", "改为", "设计", "策略", "结论", "取舍"]
    for p in pieces:
        p = re.sub(r"\s+", " ", p).strip(" -")
        if 18 <= len(p) <= 160 and any(c in p for c in cues) and not p.startswith(SYSTEM_NOTIFICATION_PREFIX):
            return p[:120]
    return cleaned[:120]


def split_runs(sess: Sess) -> list[tuple[Msg, str]]:
    runs: list[tuple[Msg, str]] = []
    for i, m in enumerate(sess.messages):
        if m.role != "user":
            continue
        following: list[str] = []
        for n in sess.messages[i + 1:]:
            if n.role == "user":
                break
            if n.role == "assistant":
                following.append(n.content)
        runs.append((m, "\n\n".join(following)))
    return runs


def extract_memories(sessions: list[Sess], max_per_session: int = 4) -> list[Memory]:
    memories: list[Memory] = []
    seen_keys: set[str] = set()
    for sess in sessions:
        runs = split_runs(sess)
        candidates: list[tuple[int, Msg, str]] = []
        for user, assistant in runs:
            text = user.content + "\n" + assistant[:4000]
            score = 0
            score += len(set(anchors(text))) * 3
            score += sum(1 for t in infer_tags(text, sess.title) if t in {"bug", "architecture", "memory", "context", "verification"}) * 2
            score += min(len(tokenize(text)) // 40, 8)
            if any(k in text for k in ["根因", "架构", "changelog", "验证", "测试", "实现", "修复", "不支持"]):
                score += 5
            candidates.append((score, user, assistant))
        candidates.sort(key=lambda x: x[0], reverse=True)
        for idx, (_, user, assistant) in enumerate(candidates[:max_per_session]):
            source_text = (user.content + "\n" + assistant).strip()
            an = anchors(source_text)
            # 标签只从用户意图和高质量事实句抽取；长 assistant 详情只做内容，不让偶然提到的词污染主题。
            fact = informative_sentence(assistant) or informative_sentence(user.content)
            tag_text = user.content + "\n" + fact
            tags = infer_tags(tag_text, sess.title)
            toks = tokenize(source_text + " " + " ".join(an) + " " + " ".join(tags))
            # 摘要以可复用事实为主，标题只是定位来源，避免“标题 + 用户一句话”把泛词放大。
            topic = normalize_title(sess.title)
            anchor_part = "、".join(an[:3]) if an else "无显式文件锚点"
            summary = f"{topic}：{fact}；关键锚点：{anchor_part}"
            key_base = re.sub(r"[^a-z0-9\u4e00-\u9fff]+", "-", (topic + "-" + "-".join(tags[:3])).lower()).strip("-")
            key = f"proj/{key_base[:70]}-{idx+1}"
            if key in seen_keys:
                key = f"{key}-{sess.id[-4:]}"
            seen_keys.add(key)
            content = "## 概览\n" + summary + "\n\n## 详情\n" + source_text[:2200]
            memories.append(Memory(
                id=key,
                summary=summary,
                content=content,
                source_session=sess.id,
                source_title=sess.title,
                kind="episode" if any(k in source_text for k in ["修复", "完成", "验证", "失败", "根因"]) else "stable",
                tags=tags,
                anchors=an,
                tokens=sorted(set(toks)),
            ))
    return memories


def strong_tokens(tokens: set[str]) -> set[str]:
    return {t for t in tokens if t in STRONG_DOMAIN_TERMS or len(t) >= 5 or re.search(r"[A-Z_/.-]", t)}


def build_links(memories: list[Memory]) -> dict[str, dict[str, float]]:
    links: dict[str, dict[str, float]] = defaultdict(dict)
    for i, a in enumerate(memories):
        a_anchors = {x.lower() for x in a.anchors}
        a_specific_tags = set(a.tags) & SPECIFIC_TAGS
        a_strong = strong_tokens(set(a.tokens))
        for b in memories[i + 1:]:
            b_anchors = {x.lower() for x in b.anchors}
            b_specific_tags = set(b.tags) & SPECIFIC_TAGS
            anchor_overlap = a_anchors & b_anchors
            tag_overlap = a_specific_tags & b_specific_tags
            token_overlap = a_strong & strong_tokens(set(b.tokens))
            if not anchor_overlap and not tag_overlap and len(token_overlap) < 2:
                continue
            score = 0.0
            score += min(len(anchor_overlap) * 0.22, 0.44)
            score += min(len(tag_overlap) * 0.14, 0.42)
            score += min(len(token_overlap) * 0.05, 0.25)
            if a.source_session == b.source_session and (anchor_overlap or tag_overlap):
                score += 0.10
            if score >= 0.26:
                w = round(min(score, 0.85), 3)
                links[a.id][b.id] = w
                links[b.id][a.id] = w
    return links


def query_topics(query: str) -> set[str]:
    low = query.lower()
    return {topic for topic, keys in TOPIC_KEYWORDS.items() if any(k in low for k in keys)}


def topic_match_score(topics: set[str], memory: Memory) -> float:
    if not topics:
        return 0.0
    tags = set(memory.tags)
    haystack = " ".join([memory.summary, " ".join(memory.tokens), " ".join(memory.anchors)]).lower()
    score = 0.0
    for topic in topics:
        if tags & TOPIC_TAGS[topic] and any(term in haystack for term in TOPIC_REQUIRED_TERMS[topic]):
            score += 1.0
    return score


def is_system_notification(text: str) -> bool:
    return text.strip().startswith(SYSTEM_NOTIFICATION_PREFIX)


def recall(query: str, memories: list[Memory], links: dict[str, dict[str, float]], max_hits: int = 5) -> list[RecallHit]:
    if is_system_notification(query):
        return []
    q_tokens = set(tokenize(query))
    q_anchors = {a.lower() for a in anchors(query)}
    q_strong = (q_tokens & STRONG_DOMAIN_TERMS) | q_anchors | strong_tokens(q_tokens)
    topics = query_topics(query)
    direct_scores: dict[str, RecallHit] = {}
    by_id = {m.id: m for m in memories}
    for m in memories:
        m_anchors = {a.lower() for a in m.anchors}
        m_specific_tags = set(m.tags) & SPECIFIC_TAGS
        mt = set(m.tokens) | m_anchors | set(m.tags)
        overlap = (q_tokens | q_anchors) & mt
        strong_overlap = q_strong & (set(m.tokens) | m_anchors | m_specific_tags)
        anchor_overlap = q_anchors & m_anchors
        topic_score = topic_match_score(topics, m)
        if topics and topic_score == 0 and not anchor_overlap:
            continue
        if not strong_overlap and not anchor_overlap and topic_score == 0:
            continue
        score = 0.0
        score += min(len(strong_overlap) * 0.07, 0.35)
        score += len(anchor_overlap) * 0.42
        score += topic_score * 0.24
        score += min(len((q_tokens & STRONG_DOMAIN_TERMS) & mt) * 0.06, 0.18)
        score += min(len((q_tokens & WEAK_DOMAIN_TERMS) & mt) * 0.01, 0.04)
        score *= max(0.25, min(m.strength, 2.0))
        if topics and not strong_overlap and not anchor_overlap:
            score -= 0.12
        if score < 0.22:
            continue
        direct_scores[m.id] = RecallHit(m.id, round(score, 4), round(score, 4), 0.0, sorted(list(overlap))[:12])
    hits = dict(direct_scores)
    for mid, hit in direct_scores.items():
        for nid, w in links.get(mid, {}).items():
            mem = by_id[nid]
            if topics and topic_match_score(topics, mem) == 0:
                continue
            spread = hit.direct * w * 0.25
            if spread < 0.09:
                continue
            old = hits.get(nid)
            reason = f"linked:{mid}:{w}"
            if old is None or spread > old.score:
                hits[nid] = RecallHit(nid, round(spread, 4), 0.0, round(spread, 4), [reason])
    ranked = sorted(hits.values(), key=lambda h: (h.score, h.direct), reverse=True)
    selected: list[RecallHit] = []
    selected_tokens: set[str] = set()
    for h in ranked:
        mt = set(by_id[h.memory_id].tokens)
        redundancy = len(mt & selected_tokens) / max(len(mt), 1)
        adjusted = h.score - 0.22 * redundancy
        if adjusted < 0.20:
            continue
        h.score = round(adjusted, 4)
        selected.append(h)
        selected_tokens |= strong_tokens(mt) | set(by_id[h.memory_id].tags)
        limit = 2 if selected[0].score < 0.34 else max_hits
        if len(selected) >= limit:
            break
    selected.sort(key=lambda h: (h.score, h.direct), reverse=True)
    return selected


def memory_from_run(sess: Sess, user: Msg, assistant: str, round_no: int) -> Memory:
    source_text = (user.content + "\n" + assistant).strip()
    an = anchors(source_text)
    fact = informative_sentence(assistant) or informative_sentence(user.content)
    tag_text = user.content + "\n" + fact
    tags = infer_tags(tag_text, sess.title)
    toks = tokenize(source_text + " " + " ".join(an) + " " + " ".join(tags))
    topic = normalize_title(sess.title)
    anchor_part = "、".join(an[:3]) if an else "无显式文件锚点"
    summary = f"{topic}：{fact}；关键锚点：{anchor_part}"
    key_base = re.sub(r"[^a-z0-9\u4e00-\u9fff]+", "-", (topic + "-" + user.id + "-learned").lower()).strip("-")
    return Memory(
        id=f"learned/{key_base[:80]}-r{round_no}",
        summary=summary,
        content="## 概览\n" + summary + "\n\n## 详情\n" + source_text[:2200],
        source_session=sess.id,
        source_title=sess.title,
        kind="episode",
        tags=tags,
        anchors=an,
        tokens=sorted(set(toks)),
        strength=1.15,
        learned_round=round_no,
    )


def grade_value(grade: str) -> int:
    return {"good": 3, "mixed": 2, "thin": 1, "miss": 0, "noisy": -2, "skipped": 0}.get(grade, 0)


def strengthen(memories: list[Memory], hits: list[RecallHit], judge: dict[str, Any]) -> None:
    by_id = {m.id: m for m in memories}
    grade = judge.get("grade")
    for h in hits:
        m = by_id.get(h.memory_id)
        if not m:
            continue
        if grade == "good":
            m.strength = min(m.strength + 0.12, 1.8)
        elif grade in {"noisy", "mixed"} and h.direct < 0.24:
            m.strength = max(m.strength - 0.18, 0.35)


def should_learn_from_run(judge: dict[str, Any], hits: list[RecallHit]) -> bool:
    grade = judge.get("grade")
    if grade in {"miss", "thin"}:
        return True
    return grade == "good" and len(hits) <= 2


def judge_recall(query: str, hits: list[RecallHit], mem_by_id: dict[str, Memory]) -> dict[str, Any]:
    if is_system_notification(query):
        return {"grade": "skipped", "too_few": False, "too_many": False, "notes": "系统通知不是用户意图，不参与记忆注入评测。"}
    q = set(tokenize(query)) | {a.lower() for a in anchors(query)}
    topics = query_topics(query)
    if not hits:
        grade = "thin" if topics else "miss"
        return {"grade": grade, "too_few": bool(topics), "too_many": False, "notes": "无注入；有明确主题时表示宁缺毋滥但可能漏召。"}
    relevant = 0
    weak = 0
    reasons = []
    for h in hits:
        m = mem_by_id[h.memory_id]
        mt = set(m.tokens) | {a.lower() for a in m.anchors} | set(m.tags)
        overlap = q & mt
        topic_ok = topic_match_score(topics, m) > 0 if topics else True
        strong_ok = bool((q & STRONG_DOMAIN_TERMS) & mt or ({a.lower() for a in anchors(query)} & {a.lower() for a in m.anchors}))
        if topic_ok and (h.direct >= 0.24 or strong_ok or len(overlap & STRONG_DOMAIN_TERMS) >= 1):
            relevant += 1
        else:
            weak += 1
            reasons.append(f"{m.id} 弱相关：{m.summary[:60]}")
    too_many = len(hits) > 5 or weak >= 2 or (len(hits) >= 4 and relevant < len(hits) - 1)
    too_few = relevant == 0 and bool(topics)
    if too_many:
        grade = "noisy"
    elif too_few:
        grade = "thin"
    elif weak:
        grade = "mixed"
    else:
        grade = "good"
    return {"grade": grade, "relevant_estimate": relevant, "weak_estimate": weak, "too_few": too_few, "too_many": too_many, "notes": "; ".join(reasons[:3])}


def collect_eval_runs(holdout: list[Sess], max_runs: int) -> list[tuple[Sess, Msg, str]]:
    eval_runs: list[tuple[Sess, Msg, str]] = []
    for sess in holdout:
        for user, assistant in split_runs(sess):
            if len(eval_runs) >= max_runs:
                return eval_runs
            if is_system_notification(user.content):
                continue
            eval_runs.append((sess, user, assistant))
    return eval_runs


def run_eval_round(
    round_no: int,
    eval_runs: list[tuple[Sess, Msg, str]],
    memories: list[Memory],
    links: dict[str, dict[str, float]],
) -> list[dict[str, Any]]:
    mem_by_id = {m.id: m for m in memories}
    evals: list[dict[str, Any]] = []
    for sess, user, assistant in eval_runs:
        query = user.content
        hits = recall(query, memories, links)
        judge = judge_recall(query, hits, mem_by_id)
        evals.append({
            "round": round_no,
            "session_id": sess.id,
            "session_title": sess.title,
            "user_message_id": user.id,
            "query": query,
            "assistant_excerpt": assistant[:900],
            "hits": [{**asdict(h), "memory": asdict(mem_by_id[h.memory_id])} for h in hits],
            "judge": judge,
            "baseline_context_excerpt": "\n\n".join(m.content for m in memories[:12])[:2500],
            "baseline": {
                "lower_bound": "无记忆新 session：不注入历史，只依赖当前用户输入。",
                "upper_bound": "历史对话原文截断：把训练会话原文按时间截断塞入上下文，信息最全但噪音与 token 成本最高。",
                "hebbian_eval": "当前脚本召回：从抽取记忆 + 稀疏关联图中选择少量候选，目标是接近上限相关性、接近下限低噪音。",
            },
        })
    return evals


def apply_learning_round(
    round_no: int,
    eval_runs: list[tuple[Sess, Msg, str]],
    memories: list[Memory],
    evals: list[dict[str, Any]],
) -> list[Memory]:
    existing = {m.id for m in memories}
    learned: list[Memory] = []
    hit_objs_by_eval: list[list[RecallHit]] = []
    for e in evals:
        hit_objs_by_eval.append([
            RecallHit(h["memory_id"], h["score"], h["direct"], h["spread"], h.get("reasons", []))
            for h in e["hits"]
        ])
    for (sess, user, assistant), e, hit_objs in zip(eval_runs, evals, hit_objs_by_eval):
        strengthen(memories, hit_objs, e["judge"])
        if should_learn_from_run(e["judge"], hit_objs):
            m = memory_from_run(sess, user, assistant, round_no)
            if m.id not in existing:
                learned.append(m)
                existing.add(m.id)
    memories.extend(learned)
    return learned


def round_metrics(evals: list[dict[str, Any]]) -> dict[str, Any]:
    grades = Counter(e["judge"]["grade"] for e in evals)
    score = sum(grade_value(e["judge"]["grade"]) for e in evals)
    injected = sum(len(e["hits"]) for e in evals)
    noisy = grades.get("noisy", 0) + grades.get("mixed", 0)
    return {
        "grades": dict(grades),
        "score": score,
        "avg_hits": round(injected / max(len(evals), 1), 3),
        "noise_runs": noisy,
        "eval_runs": len(evals),
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--sessions-dir", type=Path, default=DEFAULT_SESSIONS_DIR)
    ap.add_argument("--workdir", default=HEBBIAN_WORKDIR)
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--holdout", type=int, default=4)
    ap.add_argument("--train-limit", type=int, default=40)
    ap.add_argument("--max-runs", type=int, default=12)
    ap.add_argument("--learning-rounds", type=int, default=2, help="模拟越用越聪明的反馈回合数；0 表示只做静态评测。")
    args = ap.parse_args()

    all_sessions = read_sessions(args.sessions_dir, args.workdir)
    holdout = all_sessions[:args.holdout]
    train = all_sessions[args.holdout:args.holdout + args.train_limit]
    memories = extract_memories(train)
    eval_runs = collect_eval_runs(holdout, args.max_runs)
    all_evals: list[dict[str, Any]] = []
    learning_curve: list[dict[str, Any]] = []
    learned_memories: list[Memory] = []

    total_rounds = max(args.learning_rounds, 0)
    for round_no in range(total_rounds + 1):
        links = build_links(memories)
        evals = run_eval_round(round_no, eval_runs, memories, links)
        metrics = round_metrics(evals)
        metrics.update({
            "round": round_no,
            "memories": len(memories),
            "links": sum(len(v) for v in links.values()) // 2,
        })
        all_evals.extend(evals)
        if round_no < total_rounds:
            learned = apply_learning_round(round_no + 1, eval_runs, memories, evals)
            learned_memories.extend(learned)
            metrics["learned"] = len(learned)
        else:
            metrics["learned"] = 0
        learning_curve.append(metrics)

    final_links = build_links(memories)
    final_evals = [e for e in all_evals if e["round"] == total_rounds]

    report = {
        "design": {
            "summary": "理想系统 = 浅睡抽取高锚点记忆 + tag归一 + BM25/符号粗筛 + 图扩散 + MMR控冗余 + LLM精排后置 + 强化回写。此脚本实现可重复的启发式闭环原型。",
            "holdout_policy": "按 updated_at 取最新 holdout 个 Hebbian 项目 session，不参与初始记忆抽取。",
            "training_policy": "其余较老 session 抽取候选记忆，全部写入 target 评测产物，不污染真实 memory。",
            "learning_policy": "每轮评测后强化 good 命中；miss/thin 或少量高置信命中会从 holdout run 的 user+assistant 生成隔离 learned memory，下一轮可被召回。",
        },
        "counts": {
            "all_project_sessions": len(all_sessions),
            "holdout_sessions": len(holdout),
            "train_sessions": len(train),
            "initial_memories": len(memories) - len(learned_memories),
            "final_memories": len(memories),
            "learned_memories": len(learned_memories),
            "final_links": sum(len(v) for v in final_links.values()) // 2,
            "eval_runs": len(final_evals),
            "learning_rounds": total_rounds,
        },
        "learning_curve": learning_curve,
        "holdout": [{"id": s.id, "title": s.title, "updated_at": s.updated_at, "messages": len(s.messages)} for s in holdout],
        "train_sample": [{"id": s.id, "title": s.title, "updated_at": s.updated_at, "messages": len(s.messages)} for s in train[:12]],
        "evals": final_evals,
        "round_evals": all_evals,
    }
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "memory_recall_eval.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    (args.out / "memories.json").write_text(json.dumps([asdict(m) for m in memories], ensure_ascii=False, indent=2), encoding="utf-8")
    (args.out / "learned_memories.json").write_text(json.dumps([asdict(m) for m in learned_memories], ensure_ascii=False, indent=2), encoding="utf-8")
    summary_lines = [
        f"sessions: all={len(all_sessions)} holdout={len(holdout)} train={len(train)}",
        f"memories: initial={report['counts']['initial_memories']} learned={len(learned_memories)} final={len(memories)} links={sum(len(v) for v in final_links.values()) // 2} eval_runs={len(final_evals)}",
    ]
    for m in learning_curve:
        summary_lines.append(
            f"round {m['round']}: score={m['score']} grades={json.dumps(m['grades'], ensure_ascii=False)} "
            f"avg_hits={m['avg_hits']} noise_runs={m['noise_runs']} learned={m['learned']}"
        )
    grades = Counter(e["judge"]["grade"] for e in final_evals)
    summary_lines.append("final_grades=" + json.dumps(grades, ensure_ascii=False))
    for e in final_evals:
        summary_lines.append(f"\n## {e['session_title']} / {e['user_message_id']} / {e['judge']['grade']}")
        summary_lines.append(e["query"][:240].replace("\n", " "))
        for h in e["hits"][:5]:
            learned_mark = " learned" if h["memory"].get("learned_round", 0) else ""
            summary_lines.append(f"- {h['score']:.3f}{learned_mark} {h['memory']['id']} :: {h['memory']['summary'][:120]}")
        summary_lines.append("judge: " + json.dumps(e["judge"], ensure_ascii=False))
    (args.out / "summary.md").write_text("\n".join(summary_lines), encoding="utf-8")
    print("\n".join(summary_lines[:3 + len(learning_curve)]))
    print(f"wrote {args.out / 'memory_recall_eval.json'}")


if __name__ == "__main__":
    main()
