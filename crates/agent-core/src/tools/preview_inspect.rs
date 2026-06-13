//! PreviewInspect：内置浏览器「元素对话」旁支会话专用的样式/结构诊断工具（架构 §8.5）。
//!
//! 解「复杂样式问题」的关键：模型能查到某元素**生效的 CSS 规则链**（哪条规则、
//! 什么选择器、被谁覆盖）——这正是 DevTools 的 matched rules 视角；也能读 DOM
//! 结构（子树/兄弟），判断「用户圈的这个元素是不是一类重复结构中的一个」。
//! 读路径走 PreviewBridge（CDP）。无 bridge 时返回明确的不可用提示。

use std::sync::Arc;

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde::Deserialize;
use serde_json::Value;

use crate::preview_bridge::PreviewBridge;
use crate::tools::Tool;

pub const PREVIEW_INSPECT_TOOL_NAME: &str = "PreviewInspect";

#[derive(Debug, Deserialize)]
pub struct PreviewInspectInput {
    /// 查哪个元素：CSS selector（`@N` 由 surface 在下发前换算成 selector）。
    pub selector: String,
    /// 查什么：`rules`（生效 CSS 规则链）/ `tree`（子树结构）/ `siblings`（同级元素及同构性）。
    pub what: String,
}

pub struct PreviewInspectTool {
    bridge: Option<Arc<dyn PreviewBridge>>,
}

impl PreviewInspectTool {
    pub fn new(bridge: Option<Arc<dyn PreviewBridge>>) -> Self {
        Self { bridge }
    }
}

/// siblings/tree 用的页面内取数表达式。返回 JSON 字符串由模型直接读。
/// （raw string 用 ## 定界：JS 源码里含 `"#"`，单 # 会被它提前终止。）
fn eval_expr(selector: &str, what: &str) -> String {
    let sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    let brief = r##"
      var brief = function (e) {
        if (!e || !e.tagName) return null;
        var t = e.tagName.toLowerCase();
        if (e.id) t += "#" + e.id;
        if (e.classList && e.classList.length) t += "." + Array.prototype.slice.call(e.classList).join(".");
        var txt = (e.textContent || "").trim().slice(0, 40);
        return txt ? t + ' "' + txt + '"' : t;
      };"##;
    match what {
        "tree" => format!(
            r##"(function () {{
              {brief}
              var el = document.querySelector({sel});
              if (!el) return JSON.stringify({{ error: "no match" }});
              var walk = function (e, depth) {{
                var node = {{ el: brief(e), children: [] }};
                if (depth < 3) {{
                  for (var i = 0; i < e.children.length && i < 20; i++) node.children.push(walk(e.children[i], depth + 1));
                }}
                return node;
              }};
              return JSON.stringify(walk(el, 0), null, 1);
            }})()"##
        ),
        // siblings：同级全列 + 同构性判断（tag+class 相同的兄弟数）。
        // 隐藏元素（display:none，含 PreviewMutate remove 的草稿态）不计入同构数，
        // 避免把"已删"的元素算进"要一起改的一类"误导模型。
        _ => format!(
            r##"(function () {{
              {brief}
              var visible = function (e) {{ return e.offsetParent !== null || window.getComputedStyle(e).position === "fixed"; }};
              var el = document.querySelector({sel});
              if (!el || !el.parentElement) return JSON.stringify({{ error: "no match or no parent" }});
              var sibs = Array.prototype.slice.call(el.parentElement.children);
              var sig = function (e) {{ return e.tagName + "|" + Array.prototype.slice.call(e.classList).sort().join("."); }};
              var same = sibs.filter(function (s) {{ return sig(s) === sig(el) && visible(s); }}).length;
              var pcs = window.getComputedStyle(el.parentElement);
              return JSON.stringify({{
                parent: brief(el.parentElement),
                parentLayout: {{ display: pcs.display, flexDirection: pcs.flexDirection, gap: pcs.gap, gridTemplateColumns: pcs.gridTemplateColumns }},
                index: sibs.indexOf(el),
                total: sibs.length,
                sameStructureCount: same,
                siblings: sibs.slice(0, 16).map(function (s) {{ return (s === el ? "→ " : "") + (visible(s) ? "" : "[hidden] ") + brief(s); }})
              }}, null, 1);
            }})()"##
        ),
    }
}

#[async_trait]
impl Tool for PreviewInspectTool {
    fn name(&self) -> &str {
        PREVIEW_INSPECT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Inspect the live page preview before changing it. what=rules returns the \
         matched CSS rule chain for the element (which selectors apply, in priority \
         order — essential when a style 'doesn't work' because something overrides \
         it). what=siblings tells you whether the element is one of several \
         identically-structured siblings (sameStructureCount > 1 means the user \
         probably wants the whole group changed, not just one). what=tree returns \
         the element's subtree structure. `selector` is a CSS selector."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["selector", "what"],
            "properties": {
                "selector": { "type": "string", "description": "CSS selector of the element to inspect" },
                "what": { "type": "string", "enum": ["rules", "tree", "siblings"], "description": "rules = matched CSS rules; tree = subtree; siblings = sibling structure analysis" }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let parsed: PreviewInspectInput = serde_json::from_value(input)
            .map_err(|e| AppError::msg(format!("invalid PreviewInspect input: {e}")))?;
        let Some(bridge) = self.bridge.as_ref() else {
            return Ok(
                "当前预览不支持检查（未连接 CDP 通道）。请基于 <selected_elements> 里的快照信息判断。"
                    .to_string(),
            );
        };
        match parsed.what.as_str() {
            "rules" => bridge.matched_rules(&parsed.selector).await,
            "tree" | "siblings" => {
                bridge
                    .eval(&eval_expr(&parsed.selector, &parsed.what))
                    .await
            }
            other => Err(AppError::msg(format!(
                "未知 what: {other}（可选 rules / tree / siblings）"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeBridge;

    #[async_trait]
    impl PreviewBridge for FakeBridge {
        async fn capture(&self, _selector: Option<&str>) -> AppResult<Vec<u8>> {
            unreachable!()
        }
        async fn matched_rules(&self, selector: &str) -> AppResult<String> {
            Ok(format!("rules for {selector}"))
        }
        async fn eval(&self, expression: &str) -> AppResult<String> {
            Ok(format!("eval: {} chars", expression.len()))
        }
    }

    #[tokio::test]
    async fn rules_goes_through_bridge() {
        let tool = PreviewInspectTool::new(Some(Arc::new(FakeBridge)));
        let out = tool
            .execute(serde_json::json!({ "selector": ".btn", "what": "rules" }))
            .await
            .unwrap();
        assert_eq!(out, "rules for .btn");
    }

    #[tokio::test]
    async fn siblings_uses_eval() {
        let tool = PreviewInspectTool::new(Some(Arc::new(FakeBridge)));
        let out = tool
            .execute(serde_json::json!({ "selector": "li.item", "what": "siblings" }))
            .await
            .unwrap();
        assert!(out.starts_with("eval:"));
    }

    #[tokio::test]
    async fn no_bridge_degrades_with_clear_message() {
        let tool = PreviewInspectTool::new(None);
        let out = tool
            .execute(serde_json::json!({ "selector": ".x", "what": "rules" }))
            .await
            .unwrap();
        assert!(out.contains("不支持检查"));
    }
}
