export interface DiffRow {
  left: string;
  right: string;
  kind: "same" | "add" | "remove";
}

export interface DiffStats {
  addCount: number;
  removeCount: number;
}

export function calculateDiffRows(beforeText: string, afterText: string): DiffRow[] {
  const beforeLines = beforeText.split("\n");
  const afterLines = afterText.split("\n");
  const m = beforeLines.length;
  const n = afterLines.length;
  const dp = new Uint16Array((m + 1) * (n + 1));
  const idx = (i: number, j: number) => i * (n + 1) + j;

  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      if (beforeLines[i - 1] === afterLines[j - 1]) {
        dp[idx(i, j)] = dp[idx(i - 1, j - 1)] + 1;
      } else {
        dp[idx(i, j)] = Math.max(dp[idx(i - 1, j)], dp[idx(i, j - 1)]);
      }
    }
  }

  const rev: DiffRow[] = [];
  let i = m;
  let j = n;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && beforeLines[i - 1] === afterLines[j - 1]) {
      rev.push({ left: beforeLines[i - 1], right: afterLines[j - 1], kind: "same" });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || dp[idx(i, j - 1)] >= dp[idx(i - 1, j)])) {
      rev.push({ left: "", right: afterLines[j - 1], kind: "add" });
      j--;
    } else {
      rev.push({ left: beforeLines[i - 1], right: "", kind: "remove" });
      i--;
    }
  }
  return rev.reverse();
}

export function calculateDiffStats(beforeText: string, afterText: string): DiffStats {
  const rows = calculateDiffRows(beforeText, afterText);
  return {
    addCount: rows.filter((row) => row.kind === "add").length,
    removeCount: rows.filter((row) => row.kind === "remove").length,
  };
}
