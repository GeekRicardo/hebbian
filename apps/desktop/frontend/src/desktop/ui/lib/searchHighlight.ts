export type MatchRange = [number, number];

export interface HighlightSegment {
  text: string;
  highlighted: boolean;
}

export function findLiteralMatches(
  text: string,
  query: string,
  caseSensitive: boolean
): MatchRange[] {
  const needle = query.trim();
  if (!needle) return [];

  const haystack = caseSensitive ? text : text.toLowerCase();
  const normalizedNeedle = caseSensitive ? needle : needle.toLowerCase();
  const matches: MatchRange[] = [];
  let cursor = 0;

  while (cursor <= haystack.length - normalizedNeedle.length) {
    const index = haystack.indexOf(normalizedNeedle, cursor);
    if (index < 0) break;
    matches.push([index, index + normalizedNeedle.length]);
    cursor = index + normalizedNeedle.length;
  }

  return matches;
}

export function findRegexMatches(
  text: string,
  query: string,
  caseSensitive: boolean
): MatchRange[] {
  const pattern = query.trim();
  if (!pattern) return [];

  let regex: RegExp;
  try {
    regex = new RegExp(pattern, caseSensitive ? "g" : "gi");
  } catch {
    return [];
  }

  const matches: MatchRange[] = [];
  let match: RegExpExecArray | null;
  while ((match = regex.exec(text))) {
    if (match[0].length === 0) {
      regex.lastIndex++;
      continue;
    }
    matches.push([match.index, match.index + match[0].length]);
  }
  return matches;
}

export function findSearchMatches(
  text: string,
  query: string,
  caseSensitive: boolean,
  regex: boolean
): MatchRange[] {
  return regex
    ? findRegexMatches(text, query, caseSensitive)
    : findLiteralMatches(text, query, caseSensitive);
}

export function splitHighlightedText(
  text: string,
  matches: MatchRange[]
): HighlightSegment[] {
  if (matches.length === 0) {
    return text ? [{ text, highlighted: false }] : [];
  }

  const segments: HighlightSegment[] = [];
  let cursor = 0;

  for (const [start, end] of matches) {
    const safeStart = Math.max(cursor, Math.min(start, text.length));
    const safeEnd = Math.max(safeStart, Math.min(end, text.length));
    if (safeStart > cursor) {
      segments.push({ text: text.slice(cursor, safeStart), highlighted: false });
    }
    if (safeEnd > safeStart) {
      segments.push({ text: text.slice(safeStart, safeEnd), highlighted: true });
    }
    cursor = safeEnd;
  }

  if (cursor < text.length) {
    segments.push({ text: text.slice(cursor), highlighted: false });
  }

  return segments;
}
