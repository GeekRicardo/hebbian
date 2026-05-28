import { readFileSync } from "node:fs";

const source = readFileSync(
  new URL("./MessageBubble.tsx", import.meta.url),
  "utf8"
);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const avatarColumnMarker = "<div className=\"shrink-0 flex w-7 flex-col items-center gap-1.5\">";
const contentColumnMarker = "<div className={cn(\"flex-1 min-w-0\"";
const bodyMarker = "<div className=\"markdown text-[14px] leading-relaxed break-words\">";
const avatarColumnIndex = source.indexOf(avatarColumnMarker);
const contentColumnIndex = source.indexOf(contentColumnMarker);
const bodyIndex = source.indexOf(bodyMarker);
const animationIndex = source.indexOf("src={animations.assistantThinking}");
const avatarColumnEndIndex = source.indexOf("</div>", avatarColumnIndex);

assert(avatarColumnIndex >= 0, "MessageBubble should render an avatar column");
assert(contentColumnIndex >= 0, "MessageBubble should render a content column");
assert(bodyIndex >= 0, "MessageBubble should render a markdown body container");
assert(animationIndex >= 0, "MessageBubble should render the assistant thinking animation");
assert(
  avatarColumnIndex < avatarColumnEndIndex && avatarColumnEndIndex < contentColumnIndex,
  "avatar column should end before the content column starts"
);
assert(
  contentColumnIndex < bodyIndex && bodyIndex < animationIndex,
  "assistant thinking animation should follow the message body in the content column"
);
