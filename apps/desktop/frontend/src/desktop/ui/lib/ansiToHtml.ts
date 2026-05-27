/**
 * Minimal ANSI escape → HTML converter.
 * Handles SGR codes (colors, bold, dim, italic, underline, reset).
 * Input: raw string that may contain \x1b[...m sequences.
 * Output: HTML string safe for dangerouslySetInnerHTML inside a <pre>.
 */

const ANSI_COLORS: Record<number, string> = {
  30: "#1e1e1e", 31: "#f44747", 32: "#6a9955", 33: "#dcdcaa",
  34: "#569cd6", 35: "#c586c0", 36: "#4ec9b0", 37: "#d4d4d4",
  90: "#808080", 91: "#f44747", 92: "#b5cea8", 93: "#dcdcaa",
  94: "#9cdcfe", 95: "#c586c0", 96: "#4ec9b0", 97: "#ffffff",
};

const BG_COLORS: Record<number, string> = {
  40: "#1e1e1e", 41: "#f44747", 42: "#6a9955", 43: "#dcdcaa",
  44: "#569cd6", 45: "#c586c0", 46: "#4ec9b0", 47: "#d4d4d4",
  100: "#808080", 101: "#f44747", 102: "#b5cea8", 103: "#dcdcaa",
  104: "#9cdcfe", 105: "#c586c0", 106: "#4ec9b0", 107: "#ffffff",
};

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function ansiToHtml(input: string): string {
  // Split on ANSI CSI sequences: ESC [ ... m
  // eslint-disable-next-line no-control-regex
  const parts = input.split(/\x1b\[([0-9;]*)m/);
  if (parts.length === 1) return escapeHtml(input);

  let result = "";
  let fg = "";
  let bg = "";
  let bold = false;
  let dim = false;
  let italic = false;
  let underline = false;
  let openSpan = false;

  const flushOpen = () => {
    if (openSpan) return;
    const styles: string[] = [];
    const effectiveFg = bold && fg ? fg : fg; // bold doesn't change color in our theme
    if (effectiveFg) styles.push(`color:${effectiveFg}`);
    if (bg) styles.push(`background-color:${bg}`);
    if (bold) styles.push("font-weight:bold");
    if (dim) styles.push("opacity:0.6");
    if (italic) styles.push("font-style:italic");
    if (underline) styles.push("text-decoration:underline");
    if (styles.length > 0) {
      result += `<span style="${styles.join(";")}">`;
      openSpan = true;
    }
  };

  const flushClose = () => {
    if (openSpan) {
      result += "</span>";
      openSpan = false;
    }
  };

  for (let i = 0; i < parts.length; i++) {
    if (i % 2 === 0) {
      // Literal text
      if (parts[i]) {
        flushOpen();
        result += escapeHtml(parts[i]);
      }
    } else {
      // SGR params
      const params = parts[i] ? parts[i].split(";").map(Number) : [0];
      for (const code of params) {
        if (code === 0) {
          flushClose();
          fg = ""; bg = ""; bold = false; dim = false; italic = false; underline = false;
        } else if (code === 1) { bold = true; }
        else if (code === 2) { dim = true; }
        else if (code === 3) { italic = true; }
        else if (code === 4) { underline = true; }
        else if (code === 22) { bold = false; dim = false; }
        else if (code === 23) { italic = false; }
        else if (code === 24) { underline = false; }
        else if (ANSI_COLORS[code]) {
          flushClose();
          fg = ANSI_COLORS[code];
        }
        else if (BG_COLORS[code]) {
          flushClose();
          bg = BG_COLORS[code];
        }
        else if (code === 39) { flushClose(); fg = ""; }
        else if (code === 49) { flushClose(); bg = ""; }
      }
      // Close and reopen to apply new styles
      flushClose();
    }
  }
  flushClose();
  return result;
}
