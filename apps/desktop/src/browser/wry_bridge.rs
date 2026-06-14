//! WryEvalBridge：wry 内核（默认，无 CDP）下的预览观察通道（架构 §8.5）。
//!
//! 实现 agent-core 的 `PreviewBridge`：用 tauri 2.11 的 `eval_with_callback` 在预览
//! webview 里执行 JS 并拿回结果（结果经 callback 回传，oneshot 桥接成 async）。
//! - eval：直接执行表达式拿 JSON 结果（DOM/兄弟/computed style 等）。
//! - matched_rules：遍历 document.styleSheets 找匹配该元素的规则。wry 没有 CDP 的
//!   getMatchedStylesForNode，做不到精确 specificity 排序，但能列出作用于元素的规则
//!   来源 + computed 生效值，足够 LLM 判断「样式从哪来 / 为什么被覆盖」。
//! - capture：wry 无截图 API（wry 0.55 不提供），返回明确不可用提示。

use std::sync::Arc;

use agent_core::preview_bridge::PreviewBridge;
use async_trait::async_trait;
use common::{AppError, AppResult};

/// 持预览 webview 句柄（Clone 的轻量句柄）。
pub struct WryEvalBridge {
    webview: tauri::Webview,
}

impl WryEvalBridge {
    pub fn new(webview: tauri::Webview) -> Self {
        Self { webview }
    }

    pub fn shared(webview: tauri::Webview) -> Arc<dyn PreviewBridge> {
        Arc::new(Self::new(webview)) as Arc<dyn PreviewBridge>
    }

    /// 在预览 webview 执行 JS，等 callback 回传结果（JSON 字符串）。
    /// 包一层 IIFE + JSON.stringify 保证返回值可序列化；超时 5s。
    async fn eval_raw(&self, js: &str) -> AppResult<String> {
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let tx = std::sync::Mutex::new(Some(tx));
        // eval_with_callback 的结果是 JS 表达式求值后 JSON 序列化的字符串。
        self.webview
            .eval_with_callback(js, move |result| {
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(result);
                }
            })
            .map_err(|e| AppError::msg(format!("eval 下发失败: {e}")))?;
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(s)) => Ok(s),
            Ok(Err(_)) => Err(AppError::msg("eval 结果通道关闭")),
            Err(_) => Err(AppError::msg("eval 超时（页面无响应）")),
        }
    }
}

/// matched_rules 用的页面内 JS：遍历 styleSheets 找匹配 selector 首元素的规则。
fn matched_rules_js(selector: &str) -> String {
    let sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r##"(function () {{
          var el = document.querySelector({sel});
          if (!el) return JSON.stringify({{ error: "no match" }});
          var out = {{ selector: {sel}, matchedRules: [], inline: el.getAttribute("style") || "", computed: {{}} }};
          // 关键 computed 生效值（最终结果）
          var cs = window.getComputedStyle(el);
          ["color","background-color","font-size","font-weight","line-height","display",
           "width","height","margin","padding","border","border-radius","opacity",
           "position","flex-direction","justify-content","align-items","gap","text-align"]
            .forEach(function (p) {{ out.computed[p] = cs.getPropertyValue(p); }});
          // 遍历所有样式表，收作用于该元素的规则（来源 + 声明）
          for (var i = 0; i < document.styleSheets.length; i++) {{
            var sheet = document.styleSheets[i];
            var rules;
            try {{ rules = sheet.cssRules || sheet.rules; }} catch (e) {{ continue; }} // 跨域表读不了
            if (!rules) continue;
            for (var j = 0; j < rules.length; j++) {{
              var r = rules[j];
              if (!r.selectorText) continue;
              try {{ if (!el.matches(r.selectorText)) continue; }} catch (e) {{ continue; }}
              out.matchedRules.push({{
                selector: r.selectorText,
                css: r.style ? r.style.cssText : "",
                href: sheet.href || "(inline <style>)",
              }});
              if (out.matchedRules.length >= 40) break;
            }}
            if (out.matchedRules.length >= 40) break;
          }}
          return JSON.stringify(out, null, 1);
        }})()"##
    )
}

#[async_trait]
impl PreviewBridge for WryEvalBridge {
    async fn capture(&self, _selector: Option<&str>) -> AppResult<Vec<u8>> {
        Err(AppError::msg(
            "当前预览内核（wry）不支持截图。可改用 PreviewInspect 查样式规则 + computed 值判断效果。",
        ))
    }

    async fn matched_rules(&self, selector: &str) -> AppResult<String> {
        self.eval_raw(&matched_rules_js(selector)).await
    }

    async fn eval(&self, expression: &str) -> AppResult<String> {
        self.eval_raw(expression).await
    }
}
