// hebbian 内置浏览器注入脚本（架构 §8.5）。
//
// 运行环境双轨：
//   - Tauri 子 webview：initialization_script 注入。上行 = location.replace("heb-bridge://...")
//     被 Rust on_navigation 拦截（页面无感知）；下行 = Rust eval 调 window.__HEB_RX__(json)。
//   - hebweb iframe（降级路径，P2.5）：代理改写 HTML 注入。上下行 = window.postMessage。
//
// 结构约束：纯函数核心（__hebCore）不碰 DOM API，node 可直接 require 本文件做单测；
// DOM 薄壳（picker/overlay/styler/bridge）只做采集与转发，全部 try/catch 静默降级，
// 绝不影响宿主页面运行。仅暴露 window.__HEB_RX__ 一个全局入口。
(function () {
  "use strict";

  /* ───────────────────────── 纯函数核心（node 可测） ───────────────────────── */

  var MAX_SNAPSHOT_BYTES = 8192;

  function truncate(s, max) {
    if (typeof s !== "string") return s;
    return s.length > max ? s.slice(0, max) + "…[截断]" : s;
  }

  // chain：由近及远的层级描述 [{tag, id, classes, nthChild}, ...]
  // 生成「最短可定位」CSS 路径：遇到 id 即以其为锚截断。
  function buildSelectorPath(chain) {
    var parts = [];
    for (var i = 0; i < chain.length; i++) {
      var seg = chain[i];
      var piece = seg.tag.toLowerCase();
      if (seg.id) {
        parts.unshift(piece + "#" + seg.id);
        return parts.join(" > ");
      }
      var classes = (seg.classes || []).slice(0, 3);
      if (classes.length) piece += "." + classes.join(".");
      if (seg.nthChild > 0) piece += ":nth-child(" + seg.nthChild + ")";
      parts.unshift(piece);
    }
    return parts.join(" > ");
  }

  function buildXPath(chain) {
    var parts = [];
    for (var i = 0; i < chain.length; i++) {
      var seg = chain[i];
      parts.unshift(seg.tag.toLowerCase() + "[" + Math.max(1, seg.nthOfType || 1) + "]");
    }
    return "/" + parts.join("/");
  }

  // props 摘要：≤20 项、每值序列化后 ≤100 字符。
  function summarizeProps(props) {
    var out = {};
    if (!props || typeof props !== "object") return out;
    var keys = Object.keys(props).slice(0, 20);
    for (var i = 0; i < keys.length; i++) {
      var key = keys[i];
      if (key === "children") continue;
      var value;
      try {
        value = JSON.stringify(props[key]);
        if (value === undefined) value = String(props[key]);
      } catch (e) {
        value = "[NOT SERIALIZABLE]";
      }
      out[key] = truncate(value, 100);
    }
    return out;
  }

  // 沿 fiber.return 上行收集函数/类组件名（由近及远，≤8 层）。
  // 接受 mock 对象，node 可测。
  function componentChainFromFiber(fiber) {
    var chain = [];
    var node = fiber;
    var guard = 0;
    while (node && guard < 64 && chain.length < 8) {
      guard += 1;
      var type = node.type;
      if (typeof type === "function") {
        chain.push(type.displayName || type.name || "Anonymous");
      } else if (type && typeof type === "object") {
        // memo / forwardRef 包装
        var inner = type.type || type.render;
        if (typeof inner === "function") {
          chain.push(inner.displayName || inner.name || "Anonymous");
        }
      }
      node = node.return;
    }
    return chain;
  }

  // 最近的函数组件 props（host 元素的 fiber 往上找第一个函数组件）。
  function nearestComponentProps(fiber) {
    var node = fiber;
    var guard = 0;
    while (node && guard < 64) {
      guard += 1;
      var type = node.type;
      if (typeof type === "function") return node.memoizedProps || null;
      node = node.return;
    }
    return null;
  }

  // snapshot 体积上限：超 8KB 时按价值从低到高丢字段。
  // 优先丢体积大且次要的（computedStyles——盒模型图用 live computed 不依赖它）；
  // 保住关系（parent/siblings）与源码定位（react）到最后。
  function capSnapshot(snap) {
    var droppable = ["computedStyles", "innerText", "attributes", "childrenSummary", "siblings", "react"];
    var i = 0;
    while (JSON.stringify(snap).length > MAX_SNAPSHOT_BYTES && i < droppable.length) {
      delete snap[droppable[i]];
      i += 1;
    }
    if (JSON.stringify(snap).length > MAX_SNAPSHOT_BYTES && snap.innerText) {
      snap.innerText = truncate(snap.innerText, 120);
    }
    return snap;
  }

  function parseInMsg(raw) {
    var msg = null;
    if (typeof raw === "string") {
      try {
        msg = JSON.parse(raw);
      } catch (e) {
        return null;
      }
    } else if (raw && typeof raw === "object") {
      msg = raw;
    }
    if (!msg || msg.source !== "hebbian-host" || typeof msg.type !== "string") return null;
    return msg;
  }

  var __hebCore = {
    truncate: truncate,
    buildSelectorPath: buildSelectorPath,
    buildXPath: buildXPath,
    summarizeProps: summarizeProps,
    componentChainFromFiber: componentChainFromFiber,
    nearestComponentProps: nearestComponentProps,
    capSnapshot: capSnapshot,
    parseInMsg: parseInMsg,
    MAX_SNAPSHOT_BYTES: MAX_SNAPSHOT_BYTES,
  };

  // node 单测入口：require 本文件拿纯函数核心，不触发 DOM 薄壳。
  if (typeof module !== "undefined" && module.exports) {
    module.exports = __hebCore;
    return;
  }
  if (typeof window === "undefined" || window.__HEB_INSTALLED__) return;
  window.__HEB_INSTALLED__ = true;

  /* ───────────────────────── bridge：双轨传输 ───────────────────────── */

  var IN_IFRAME = window.parent && window.parent !== window;
  var sendQueue = [];
  var sending = false;

  // wry 模式上行用导航拦截：连续 location.replace 太密会互相取消，
  // 串行队列 + 40ms 间隔保证每条都被 on_navigation 看到。
  function pumpQueue() {
    if (sending || sendQueue.length === 0) return;
    sending = true;
    var msg = sendQueue.shift();
    try {
      if (IN_IFRAME) {
        window.parent.postMessage(msg, "*");
      } else {
        var encoded = encodeURIComponent(JSON.stringify(msg));
        window.location.replace("heb-bridge://msg/?d=" + encoded);
      }
    } catch (e) {
      /* 页面卸载中等场景，静默 */
    }
    setTimeout(function () {
      sending = false;
      pumpQueue();
    }, IN_IFRAME ? 0 : 40);
  }

  function send(type, payload) {
    sendQueue.push({ source: "hebbian-inspector", type: type, payload: payload || {} });
    pumpQueue();
  }

  /* ───────────────────────── overlay ───────────────────────── */

  var OVERLAY_ATTR = "data-hebbian-overlay";

  function makeOverlay(kind) {
    var el = document.createElement("div");
    el.setAttribute(OVERLAY_ATTR, kind);
    var style = el.style;
    style.position = "fixed";
    style.zIndex = "2147483646";
    style.pointerEvents = "none";
    style.display = "none";
    style.boxSizing = "border-box";
    style.borderRadius = "2px";
    if (kind === "hover") {
      style.background = "rgba(59,130,246,0.12)";
      style.border = "2px solid rgba(59,130,246,0.85)";
    } else {
      style.background = "transparent";
      style.border = "2px dashed rgba(16,24,40,0.6)";
    }
    var label = document.createElement("div");
    label.setAttribute(OVERLAY_ATTR, kind + "-label");
    var ls = label.style;
    ls.position = "absolute";
    ls.top = "-22px";
    ls.left = "0";
    ls.maxWidth = "320px";
    ls.overflow = "hidden";
    ls.whiteSpace = "nowrap";
    ls.textOverflow = "ellipsis";
    ls.font = "11px/16px -apple-system, system-ui, sans-serif";
    ls.padding = "1px 6px";
    ls.borderRadius = "4px";
    ls.background = kind === "hover" ? "rgba(59,130,246,0.95)" : "rgba(16,24,40,0.85)";
    ls.color = "#fff";
    el.appendChild(label);
    document.documentElement.appendChild(el);
    return el;
  }

  var hoverOverlay = null;
  var selectedOverlay = null;
  var hoverTarget = null;
  var selectedTarget = null;
  var overlaysHidden = false;
  var rafId = 0;
  var lastDraw = 0;

  function overlayLabelText(el) {
    var text = el.tagName.toLowerCase();
    if (el.id) text += "#" + el.id;
    else if (el.classList && el.classList.length) text += "." + el.classList[0];
    var chain = reactChainOf(el);
    if (chain.length) text += "  ⟨" + chain[0] + "⟩";
    return text;
  }

  function positionOverlay(overlay, target) {
    if (!overlay) return;
    if (!target || overlaysHidden || !target.isConnected) {
      overlay.style.display = "none";
      return;
    }
    var rect = target.getBoundingClientRect();
    overlay.style.display = "block";
    overlay.style.left = rect.left + "px";
    overlay.style.top = rect.top + "px";
    overlay.style.width = rect.width + "px";
    overlay.style.height = rect.height + "px";
    var label = overlay.firstChild;
    if (label) {
      label.textContent = overlayLabelText(target);
      label.style.top = rect.top < 26 ? "100%" : "-22px";
    }
  }

  function tick(ts) {
    if (ts - lastDraw >= 100) {
      lastDraw = ts;
      positionOverlay(hoverOverlay, hoverTarget);
      positionOverlay(selectedOverlay, selectedTarget);
    }
    rafId = window.requestAnimationFrame(tick);
  }

  function ensureOverlayLoop() {
    if (!hoverOverlay) hoverOverlay = makeOverlay("hover");
    if (!selectedOverlay) selectedOverlay = makeOverlay("selected");
    if (!rafId) rafId = window.requestAnimationFrame(tick);
  }

  /* ───────────────────────── react fiber 采集 ───────────────────────── */

  function fiberOf(el) {
    try {
      var keys = Object.getOwnPropertyNames(el);
      for (var i = 0; i < keys.length; i++) {
        if (keys[i].indexOf("__reactFiber$") === 0 || keys[i].indexOf("__reactInternalInstance$") === 0) {
          return el[keys[i]] || null;
        }
      }
    } catch (e) {
      /* 静默 */
    }
    return null;
  }

  function reactChainOf(el) {
    var node = el;
    var guard = 0;
    // fiber 可能挂在祖先元素上（文本节点等），向上最多找 3 层
    while (node && guard < 3) {
      var fiber = fiberOf(node);
      if (fiber) return componentChainFromFiber(fiber);
      node = node.parentElement;
      guard += 1;
    }
    return [];
  }

  // React dev 模式的源码位置（babel jsx-source 注入 _debugSource = {fileName,lineNumber}）。
  // 这是给主对话精确定位源码的金钥匙——沿 _debugOwner / return 上行找最近的有效位置。
  function extractDebugSource(fiber) {
    var node = fiber, guard = 0;
    while (node && guard < 24) {
      guard += 1;
      var src = node._debugSource;
      if (src && src.fileName) {
        var f = String(src.fileName);
        // 路径去前缀只留项目相对部分（src/... 或最后两三段），方便 grep
        var m = f.match(/(?:^|\/)((?:src|app|components|pages|apps)\/.*)$/);
        return { file: m ? m[1] : f.split("/").slice(-3).join("/"), line: src.lineNumber || null };
      }
      node = node._debugOwner || node.return;
    }
    return null;
  }

  /* ───────────────────────── snapshot 采集 ───────────────────────── */

  var STYLE_WHITELIST = [
    // 文字
    "font-family", "font-size", "font-weight", "line-height", "letter-spacing", "color", "text-align",
    // 盒模型
    "width", "height", "margin-top", "margin-right", "margin-bottom", "margin-left",
    "padding-top", "padding-right", "padding-bottom", "padding-left", "gap",
    // 边框背景
    "border-radius", "border-width", "border-style", "border-color",
    "background-color", "box-shadow", "opacity",
    // 盒模型简写（页面内卡片用）
    "padding", "margin",
    // 布局
    "display", "position", "flex-direction", "justify-content", "align-items",
  ];

  function domChainOf(el) {
    var chain = [];
    var node = el;
    var guard = 0;
    while (node && node.tagName && node.tagName !== "HTML" && guard < 24) {
      guard += 1;
      var nthChild = 0;
      var nthOfType = 0;
      var sib = node;
      while (sib) {
        nthChild += 1;
        if (sib.tagName === node.tagName) nthOfType += 1;
        sib = sib.previousElementSibling;
      }
      chain.push({
        tag: node.tagName,
        id: node.id || "",
        classes: node.classList ? Array.prototype.slice.call(node.classList) : [],
        nthChild: nthChild,
        nthOfType: nthOfType,
      });
      node = node.parentElement;
    }
    return chain;
  }

  function collectSnapshot(el) {
    var chain = domChainOf(el);
    var rect = el.getBoundingClientRect();
    var attributes = {};
    var attrCount = 0;
    for (var i = 0; i < el.attributes.length && attrCount < 15; i++) {
      var attr = el.attributes[i];
      if (attr.name === "style" || attr.name.indexOf("data-hebbian") === 0) continue;
      attributes[attr.name] = truncate(attr.value, 200);
      attrCount += 1;
    }
    var computed = {};
    try {
      var cs = window.getComputedStyle(el);
      for (var j = 0; j < STYLE_WHITELIST.length; j++) {
        computed[STYLE_WHITELIST[j]] = cs.getPropertyValue(STYLE_WHITELIST[j]);
      }
    } catch (e) {
      computed = {};
    }
    var fiber = fiberOf(el) || (el.parentElement ? fiberOf(el.parentElement) : null);
    var react = null;
    if (fiber) {
      react = {
        componentChain: componentChainFromFiber(fiber),
        props: summarizeProps(nearestComponentProps(fiber)),
      };
      var sourceAttr = el.getAttribute("data-source") || el.getAttribute("data-inspector-relative-path");
      if (sourceAttr) react.sourceHint = truncate(sourceAttr, 200);
      var dbg = extractDebugSource(fiber);
      if (dbg) react.source = dbg; // {file, line} —— dev 模式精确源码位置
    }
    // 元素摘要（给关系上下文用）：tag + class[0] + 短文本
    var briefEl = function (e) {
      if (!e || !e.tagName) return null;
      var t = e.tagName.toLowerCase();
      if (e.id) t += "#" + e.id;
      else if (e.classList && e.classList.length) t += "." + e.classList[0];
      var txt = "";
      try {
        for (var n = 0; n < e.childNodes.length; n++) if (e.childNodes[n].nodeType === 3) txt += e.childNodes[n].nodeValue;
        txt = txt.trim();
      } catch (x) {}
      return txt ? t + ' "' + truncate(txt, 40) + '"' : t;
    };
    var children = [];
    for (var k = 0; k < el.children.length && k < 12; k++) children.push(briefEl(el.children[k]));
    // 直接文本（不含子元素文本）——比 innerText 更适合 grep 定位
    var ownText = "";
    try {
      for (var t = 0; t < el.childNodes.length; t++) {
        if (el.childNodes[t].nodeType === 3) ownText += el.childNodes[t].nodeValue;
      }
      ownText = ownText.trim();
    } catch (e) {}
    // 兄弟元素 + 自己在父中的位置（改「与其他元素关系」必需：对齐 / 间距 / 排列顺序）
    var siblings;
    var indexInParent;
    if (el.parentElement) {
      var sibs = el.parentElement.children;
      siblings = [];
      for (var s = 0; s < sibs.length && s < 16; s++) {
        if (sibs[s] === el) indexInParent = s;
        siblings.push((sibs[s] === el ? "→ " : "") + briefEl(sibs[s]));
      }
    }
    // 父容器布局（决定子元素怎么排——flex/grid/对齐/间距）
    var parentInfo;
    if (el.parentElement) {
      var pcs = window.getComputedStyle(el.parentElement);
      parentInfo = {
        tagName: el.parentElement.tagName.toLowerCase(),
        classList: Array.prototype.slice.call(el.parentElement.classList || [], 0, 5),
        id: el.parentElement.id || undefined,
        layout: {
          display: pcs.display,
          flexDirection: pcs.flexDirection,
          justifyContent: pcs.justifyContent,
          alignItems: pcs.alignItems,
          gap: pcs.gap,
          gridTemplateColumns: pcs.gridTemplateColumns,
        },
        childCount: el.parentElement.children.length,
      };
    }
    var snap = {
      url: window.location.href,
      viewport: { width: window.innerWidth, height: window.innerHeight },
      capturedAt: new Date().toISOString(),
      tagName: el.tagName.toLowerCase(),
      id: el.id || undefined,
      classList: el.classList ? Array.prototype.slice.call(el.classList, 0, 10) : [],
      selectorPath: buildSelectorPath(chain),
      xpath: buildXPath(chain),
      attributes: attributes,
      ownText: ownText ? truncate(ownText, 120) : undefined, // 元素自身文本（最佳 grep 锚）
      innerText: truncate(el.innerText || "", 500),
      react: react,
      boundingClientRect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
      computedStyles: computed,
      parent: parentInfo,
      indexInParent: indexInParent,
      siblings: siblings,
      childrenSummary: children,
    };
    return capSnapshot(snap);
  }

  /* ───────────────────────── styler ───────────────────────── */

  var styleDiff = {}; // prop -> { before, after }

  // 取当前可改的元素：selectedTarget detach（React 等重渲染换了 DOM 节点）时，
  // 用 snapshot 的 selector/xpath 找回最新节点——否则改的是不在文档里的旧节点，看不到效果。
  function currentTarget() {
    if (selectedTarget && selectedTarget.isConnected) return selectedTarget;
    if (cardSnapshot) {
      var el = null;
      try { if (cardSnapshot.selectorPath) el = document.querySelector(cardSnapshot.selectorPath); } catch (e) {}
      if (!el && cardSnapshot.xpath) {
        try { el = document.evaluate(cardSnapshot.xpath, document, null, 9, null).singleNodeValue; } catch (e) {}
      }
      if (el) selectedTarget = el;
    }
    return selectedTarget;
  }

  function styleSet(prop, value, allowAny) {
    var el = currentTarget();
    if (!el) return;
    // CARD_FIELDS 走白名单（防误操作）；盒模型图 / 全部 CSS 列表走 allowAny（任意属性，
    // 改 inline style 不执行代码、安全）。
    if (!allowAny && STYLE_WHITELIST.indexOf(prop) === -1) return;
    try {
      if (!(prop in styleDiff)) {
        styleDiff[prop] = { before: el.style.getPropertyValue(prop), after: value };
      } else {
        styleDiff[prop].after = value;
      }
      el.style.setProperty(prop, value);
      // 改边框宽度/颜色但 border-style 为 none 时看不到——自动补 solid
      if ((/border.*width/.test(prop) || prop === "border-color") &&
          window.getComputedStyle(el).borderStyle === "none") {
        el.style.setProperty("border-style", "solid");
      }
    } catch (e) {
      /* 静默 */
    }
  }
  function styleApply(prop, value) { styleSet(prop, value, false); }
  function styleApplyAny(prop, value) { styleSet(prop, value, true); }

  function styleRevert() {
    var el = currentTarget();
    if (!el) {
      styleDiff = {};
      return;
    }
    try {
      var props = Object.keys(styleDiff);
      for (var i = props.length - 1; i >= 0; i--) {
        var prop = props[i];
        var before = styleDiff[prop].before;
        if (before) el.style.setProperty(prop, before);
        else el.style.removeProperty(prop);
      }
    } catch (e) {
      /* 静默 */
    }
    styleDiff = {};
  }

  function takeStyleDiff() {
    var out = [];
    var props = Object.keys(styleDiff);
    for (var i = 0; i < props.length; i++) {
      var prop = props[i];
      if (styleDiff[prop].before !== styleDiff[prop].after) {
        out.push({ prop: prop, before: styleDiff[prop].before || "(默认)", after: styleDiff[prop].after });
      }
    }
    return out;
  }

  /* ─────────────────────── 页面内注释卡片（vanilla DOM） ───────────────────────
     渲染在页面自身 DOM 里——embedded 子 webview 与 popout 独立窗口共用同一套，
     不被原生 webview 遮挡，且就在元素旁边。提交时把 {snapshot, comment, styleDiff}
     经上行通道发回宿主，由主窗口 React 组装成 user message 发进对话。 */

  var cardEl = null;
  var cardSnapshot = null;
  // 元素对话（旁支会话）状态：按元素 key 存会话 + 历史，页面没刷新就一直在
  var asideKeyCounter = 0;
  var asideConvos = {}; // elementKey -> { sessionId, messages: [{role,text}] }
  var cardChat = null;  // 当前卡片打开的聊天：{ elementKey, sessionId, msgList, assistantRow }
  // 修改队列：多个元素的待提交改动按元素累积，统一提交到主对话
  var editQueue = []; // [{ elementKey, badge, snapshot, styleDiff }]
  var queuePanelEl = null;
  var queuePos = null; // 拖动后记住的位置

  function elementKeyOf(el) {
    if (!el) return "el-0";
    if (!el.__hebAsideKey__) el.__hebAsideKey__ = "el-" + (++asideKeyCounter);
    return el.__hebAsideKey__;
  }

  // 样式编辑器字段（对齐用户截图：字号/字重/颜色/圆角/边框/间距）
  // 视觉属性（盒子尺寸 margin/border/padding 四边由盒模型图精确管，这里不重复，避免不一致）
  var CARD_FIELDS = [
    { prop: "width", label: "宽度", kind: "px" },
    { prop: "height", label: "高度", kind: "px" },
    { prop: "font-size", label: "字号", kind: "px" },
    { prop: "font-weight", label: "字重", kind: "select", options: ["300", "400", "500", "600", "700", "800"] },
    { prop: "line-height", label: "行高", kind: "px" },
    { prop: "letter-spacing", label: "字距", kind: "px" },
    { prop: "color", label: "文字颜色", kind: "color" },
    { prop: "text-align", label: "对齐", kind: "select", options: ["left", "center", "right", "justify"] },
    { prop: "background-color", label: "背景色", kind: "color" },
    { prop: "border-radius", label: "圆角", kind: "px" },
    { prop: "border-color", label: "边框颜色", kind: "color" },
    { prop: "opacity", label: "透明度", kind: "text" },
    { prop: "display", label: "显示", kind: "select", options: ["block", "inline-block", "flex", "inline-flex", "grid", "inline", "none"] },
    { prop: "justify-content", label: "主轴对齐", kind: "select", options: ["flex-start", "center", "flex-end", "space-between", "space-around"] },
    { prop: "align-items", label: "交叉对齐", kind: "select", options: ["stretch", "flex-start", "center", "flex-end", "baseline"] },
  ];

  function readComputed(prop) {
    var el = currentTarget();
    if (!el) return "";
    try {
      var cs = window.getComputedStyle(el);
      var v = cs.getPropertyValue(prop);
      // border-color 简写 computed 常返回空，回退到 -top-color（盒模型已管 border 宽度，这里只调色）
      if ((!v || v === "") && prop === "border-color") v = cs.getPropertyValue("border-top-color");
      return v || "";
    } catch (e) {
      return "";
    }
  }

  function pxNumber(raw) {
    var m = String(raw).match(/^(-?\d+(?:\.\d+)?)/);
    return m ? m[1] : "";
  }

  function rgbToHex(raw) {
    var m = String(raw).match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
    if (!m) return /^#[0-9a-fA-F]{6}$/.test(String(raw).trim()) ? String(raw).trim() : "#000000";
    var h = function (n) { return ("0" + Number(n).toString(16)).slice(-2); };
    return "#" + h(m[1]) + h(m[2]) + h(m[3]);
  }

  function cardRow(field) {
    var row = document.createElement("label");
    row.style.cssText = "display:flex;align-items:center;gap:8px;font-size:12px;margin-bottom:6px;";
    var name = document.createElement("span");
    name.textContent = field.label;
    name.style.cssText = "width:64px;flex:none;color:#57606a;";
    row.appendChild(name);
    var raw = readComputed(field.prop);
    var input;
    if (field.kind === "color") {
      // 方块色板 + # + 6 位 hex 文本框（双向同步）
      var cwrap = document.createElement("span");
      cwrap.style.cssText = "flex:1;display:flex;align-items:center;gap:6px;";
      var swatch = document.createElement("input");
      swatch.type = "color";
      swatch.value = rgbToHex(raw);
      swatch.style.cssText = "width:24px;height:24px;flex:none;padding:0;border:1px solid #d9dde3;border-radius:4px;background:#fff;cursor:pointer;";
      var hash = document.createElement("span");
      hash.textContent = "#";
      hash.style.cssText = "color:#8c949e;font-size:12px;flex:none;";
      var hex = document.createElement("input");
      hex.type = "text";
      hex.maxLength = 6;
      hex.value = rgbToHex(raw).replace("#", "");
      hex.style.cssText = "flex:1;min-width:0;height:24px;background:#f6f8fa;color:#1f2328;border:1px solid #d9dde3;border-radius:4px;font:12px ui-monospace,monospace;padding:0 8px;box-sizing:border-box;outline:none;";
      swatch.addEventListener("input", function () { hex.value = swatch.value.replace("#", ""); styleApply(field.prop, swatch.value); });
      hex.addEventListener("input", function () {
        var v = hex.value.replace(/[^0-9a-fA-F]/g, "").slice(0, 6);
        hex.value = v;
        if (v.length === 6) { swatch.value = "#" + v; styleApply(field.prop, "#" + v); }
      });
      cwrap.appendChild(swatch); cwrap.appendChild(hash); cwrap.appendChild(hex);
      row.appendChild(cwrap);
      return row;
    } else if (field.kind === "select") {
      input = document.createElement("select");
      input.style.cssText = "flex:1;height:24px;background:#f6f8fa;color:#1f2328;border:1px solid #d9dde3;border-radius:4px;font-size:12px;";
      var empty = document.createElement("option"); empty.value = ""; empty.textContent = "—"; input.appendChild(empty);
      for (var i = 0; i < field.options.length; i++) {
        var o = document.createElement("option"); o.value = field.options[i]; o.textContent = field.options[i];
        if (String(raw).trim() === field.options[i]) o.selected = true;
        input.appendChild(o);
      }
      input.addEventListener("change", function () { if (input.value) styleApply(field.prop, input.value); });
    } else if (field.kind === "text") {
      // 自由文本（opacity / 复杂值）——原值应用，不加 px
      input = document.createElement("input");
      input.type = "text";
      input.value = String(raw).trim();
      input.style.cssText = "flex:1;min-width:0;height:24px;background:#f6f8fa;color:#1f2328;border:1px solid #d9dde3;border-radius:4px;font:12px ui-monospace,monospace;padding:0 8px;box-sizing:border-box;outline:none;";
      input.addEventListener("input", function () { styleApply(field.prop, input.value); });
    } else {
      var wrap = document.createElement("span");
      wrap.style.cssText = "flex:1;display:flex;align-items:center;gap:4px;";
      input = document.createElement("input");
      input.type = "number";
      input.value = pxNumber(raw);
      input.style.cssText = "width:100%;height:24px;background:#f6f8fa;color:#1f2328;border:1px solid #d9dde3;border-radius:4px;font-size:12px;padding:0 6px;box-sizing:border-box;";
      input.addEventListener("input", function () { styleApply(field.prop, input.value + "px"); });
      var unit = document.createElement("span"); unit.textContent = "px"; unit.style.cssText = "color:#6e7681;font-size:10px;";
      wrap.appendChild(input); wrap.appendChild(unit);
      row.appendChild(wrap);
      return row;
    }
    row.appendChild(input);
    return row;
  }

  function removeCard() {
    if (cardEl && cardEl.parentNode) cardEl.parentNode.removeChild(cardEl);
    cardEl = null;
    cardSnapshot = null;
    if (typeof removeBoxRegion === "function") removeBoxRegion(); // 清盒模型 hover 高亮残留
  }

  // 拖动卡片：按住 handle 移动 card（改 left/top，清掉 right 定位）
  function makeCardDraggable(card, handle) {
    handle.addEventListener("mousedown", function (e) {
      if (e.target && e.target.tagName === "BUTTON") return; // 关闭按钮不触发拖动
      e.preventDefault();
      var rect = card.getBoundingClientRect();
      card.style.left = rect.left + "px";
      card.style.top = rect.top + "px";
      card.style.right = "auto";
      var startX = e.clientX, startY = e.clientY, baseL = rect.left, baseT = rect.top;
      var onMove = function (ev) {
        var l = Math.max(0, Math.min(window.innerWidth - 40, baseL + ev.clientX - startX));
        var t = Math.max(0, Math.min(window.innerHeight - 24, baseT + ev.clientY - startY));
        card.style.left = l + "px";
        card.style.top = t + "px";
      };
      var onUp = function () {
        document.removeEventListener("mousemove", onMove, true);
        document.removeEventListener("mouseup", onUp, true);
      };
      document.addEventListener("mousemove", onMove, true);
      document.addEventListener("mouseup", onUp, true);
    });
  }

  // ─────────────────────── 修改队列框（可拖动，统一提交） ───────────────────────
  function addToQueue(elementKey, badge, snapshot, styleDiff) {
    if (!styleDiff || !styleDiff.length) return;
    var existing = null;
    for (var i = 0; i < editQueue.length; i++) if (editQueue[i].elementKey === elementKey) existing = editQueue[i];
    if (existing) { existing.snapshot = snapshot; existing.styleDiff = styleDiff; existing.badge = badge; }
    else editQueue.push({ elementKey: elementKey, badge: badge, snapshot: snapshot, styleDiff: styleDiff });
    renderQueuePanel();
  }

  function removeQueueItem(elementKey) {
    editQueue = editQueue.filter(function (q) { return q.elementKey !== elementKey; });
    renderQueuePanel();
  }

  function makeQueueDraggable(panel, handle) {
    handle.addEventListener("mousedown", function (e) {
      if (e.target && e.target.tagName === "BUTTON") return;
      e.preventDefault();
      var rect = panel.getBoundingClientRect();
      panel.style.left = rect.left + "px"; panel.style.top = rect.top + "px";
      panel.style.right = "auto"; panel.style.bottom = "auto";
      var sx = e.clientX, sy = e.clientY, bl = rect.left, bt = rect.top;
      var onMove = function (ev) {
        var l = Math.max(0, Math.min(window.innerWidth - 40, bl + ev.clientX - sx));
        var t = Math.max(0, Math.min(window.innerHeight - 24, bt + ev.clientY - sy));
        panel.style.left = l + "px"; panel.style.top = t + "px";
        queuePos = { x: l, y: t };
      };
      var onUp = function () {
        document.removeEventListener("mousemove", onMove, true);
        document.removeEventListener("mouseup", onUp, true);
      };
      document.addEventListener("mousemove", onMove, true);
      document.addEventListener("mouseup", onUp, true);
    });
  }

  function renderQueuePanel() {
    if (!editQueue.length) {
      if (queuePanelEl && queuePanelEl.parentNode) queuePanelEl.parentNode.removeChild(queuePanelEl);
      queuePanelEl = null;
      return;
    }
    if (!queuePanelEl) {
      queuePanelEl = document.createElement("div");
      queuePanelEl.setAttribute(OVERLAY_ATTR, "queue");
      var pos = queuePos ? "left:" + queuePos.x + "px;top:" + queuePos.y + "px;" : "right:16px;bottom:16px;";
      queuePanelEl.style.cssText = "position:fixed;" + pos + "width:288px;max-height:62vh;display:flex;flex-direction:column;z-index:2147483646;" +
        "background:#fff;color:#1f2328;border:1px solid #d9dde3;border-radius:10px;box-shadow:0 8px 30px rgba(15,23,42,0.18);" +
        "font-family:-apple-system,system-ui,sans-serif;overflow:hidden;";
      queuePanelEl.addEventListener("click", function (e) { e.stopPropagation(); }, false);
      queuePanelEl.addEventListener("mousedown", function (e) { e.stopPropagation(); }, false);
      document.documentElement.appendChild(queuePanelEl);
    }
    queuePanelEl.innerHTML = "";
    var head = document.createElement("div");
    head.style.cssText = "display:flex;align-items:center;gap:6px;padding:8px 10px;border-bottom:1px solid #d9dde3;cursor:move;user-select:none;font-size:12px;font-weight:500;";
    head.textContent = "修改队列 (" + editQueue.length + ")";
    makeQueueDraggable(queuePanelEl, head);
    var list = document.createElement("div");
    list.style.cssText = "flex:1;overflow-y:auto;padding:6px 8px;";
    editQueue.forEach(function (item) {
      var row = document.createElement("div");
      row.style.cssText = "padding:6px 0;border-bottom:1px solid #f0f2f4;";
      var top = document.createElement("div");
      top.style.cssText = "display:flex;align-items:center;gap:6px;";
      var b = document.createElement("span");
      b.textContent = item.badge;
      b.style.cssText = "flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font:11px ui-monospace,monospace;color:#1f2328;";
      var del = document.createElement("button");
      del.textContent = "×";
      del.style.cssText = "border:none;background:none;color:#8c949e;font-size:15px;line-height:1;cursor:pointer;padding:0 2px;";
      del.addEventListener("click", function () { removeQueueItem(item.elementKey); });
      top.appendChild(b); top.appendChild(del);
      var diff = document.createElement("div");
      diff.style.cssText = "margin-top:2px;font:10px ui-monospace,monospace;color:#6e7681;white-space:pre-wrap;";
      diff.textContent = item.styleDiff.map(function (d) { return d.prop + ": " + d.before + " → " + d.after; }).join("\n");
      row.appendChild(top); row.appendChild(diff);
      list.appendChild(row);
    });
    var foot = document.createElement("div");
    foot.style.cssText = "display:flex;justify-content:space-between;gap:8px;padding:8px 10px;border-top:1px solid #d9dde3;";
    var clear = mkFlatBtn("清空");
    clear.addEventListener("click", function () { editQueue = []; renderQueuePanel(); });
    var submit = mkPrimaryBtn("提交到主对话");
    submit.addEventListener("click", function () {
      send("heb:annotation:submit-batch", {
        items: editQueue.map(function (q) { return { snapshot: q.snapshot, styleDiff: q.styleDiff }; }),
      });
      editQueue = [];
      renderQueuePanel();
    });
    foot.appendChild(clear); foot.appendChild(submit);
    queuePanelEl.appendChild(head); queuePanelEl.appendChild(list); queuePanelEl.appendChild(foot);
  }

  function elementBadge(snap) {
    var t = snap.tagName;
    if (snap.id) t += "#" + snap.id;
    else if (snap.classList && snap.classList.length) t += "." + snap.classList[0];
    var comp = snap.react && snap.react.componentChain && snap.react.componentChain[0];
    return comp ? t + "  ⟨" + comp + "⟩" : t;
  }

  // 给旁支 LLM / 主对话的完整元素定位描述——尽量多锚点让源码定位精确
  function elementLocator(snap) {
    var lines = [elementBadge(snap)];
    if (snap.react && snap.react.source) {
      lines.push("源码位置: " + snap.react.source.file + (snap.react.source.line ? ":" + snap.react.source.line : ""));
    }
    if (snap.react && snap.react.componentChain && snap.react.componentChain.length) {
      lines.push("React 组件链(近→远): " + snap.react.componentChain.join(" > "));
    }
    if (snap.react && snap.react.props && Object.keys(snap.react.props).length) {
      lines.push("组件 props: " + JSON.stringify(snap.react.props));
    }
    if (snap.ownText) lines.push('元素文本: "' + snap.ownText + '"');
    if (snap.id) lines.push("id: #" + snap.id);
    if (snap.classList && snap.classList.length) lines.push("class: " + snap.classList.join(" "));
    if (snap.attributes) {
      var attrKeys = Object.keys(snap.attributes).filter(function (k) { return k !== "class" && k !== "id"; });
      if (attrKeys.length) lines.push("属性: " + attrKeys.map(function (k) { return k + '="' + snap.attributes[k] + '"'; }).join(" "));
    }
    if (snap.selectorPath) lines.push("CSS 路径: " + snap.selectorPath);
    // 与周围元素的关系（改对齐 / 间距 / 排列顺序 / 增删时必需）
    if (snap.parent) {
      var p = snap.parent;
      var pd = p.tagName + (p.id ? "#" + p.id : (p.classList && p.classList.length ? "." + p.classList[0] : ""));
      var lay = "";
      if (p.layout) {
        lay = " [" + p.layout.display;
        if (p.layout.display === "flex") {
          lay += " " + p.layout.flexDirection + " justify:" + p.layout.justifyContent + " align:" + p.layout.alignItems;
        } else if (p.layout.display === "grid") {
          lay += " cols:" + p.layout.gridTemplateColumns;
        }
        if (p.layout.gap && p.layout.gap !== "normal" && p.layout.gap !== "0px") lay += " gap:" + p.layout.gap;
        lay += "]";
      }
      lines.push("父容器: " + pd + lay + "（共 " + (p.childCount != null ? p.childCount : "?") + " 个直接子元素）");
    }
    if (snap.siblings && snap.siblings.length > 1) {
      lines.push("同级元素（→ 标记的是当前，第 " + ((snap.indexInParent || 0) + 1) + "/" + snap.siblings.length + " 个）: " + snap.siblings.join(" ｜ "));
    }
    if (snap.childrenSummary && snap.childrenSummary.length) {
      lines.push("它的子元素: " + snap.childrenSummary.join(" ｜ "));
    }
    return lines.join("\n");
  }

  // ── Chrome F12 式盒模型图（margin/border/padding/content 嵌套，每个数字可改立即生效）──
  function boxNumInput(prop) {
    var el = currentTarget();
    var raw = el ? Math.round(parseFloat(window.getComputedStyle(el).getPropertyValue(prop)) || 0) : 0;
    var inp = document.createElement("input");
    inp.type = "text";
    inp.value = String(raw);
    inp.style.cssText = "width:26px;border:none;background:transparent;text-align:center;font:11px ui-monospace,monospace;color:#1f2328;outline:none;cursor:text;";
    inp.addEventListener("focus", function () { inp.style.background = "rgba(255,255,255,0.7)"; inp.select(); });
    inp.addEventListener("blur", function () { inp.style.background = "transparent"; });
    inp.addEventListener("input", function () {
      var v = inp.value.trim();
      var n = parseFloat(v);
      styleApplyAny(prop, isNaN(n) ? v : n + "px");
    });
    return inp;
  }

  function boxLayer(color, label, topP, rightP, bottomP, leftP) {
    var layer = document.createElement("div");
    layer.setAttribute("data-box-region", label); // hover 时高亮元素对应区域
    layer.style.cssText = "position:relative;background:" + color + ";border-radius:3px;padding:16px 30px;display:flex;align-items:center;justify-content:center;";
    var lab = document.createElement("span");
    lab.textContent = label;
    lab.style.cssText = "position:absolute;top:2px;left:5px;font-size:9px;color:#6b5a3e;text-transform:lowercase;";
    layer.appendChild(lab);
    var mk = function (prop, css) { var w = document.createElement("span"); w.style.cssText = "position:absolute;" + css; w.appendChild(boxNumInput(prop)); layer.appendChild(w); };
    mk(topP, "top:1px;left:50%;transform:translateX(-50%);");
    mk(bottomP, "bottom:1px;left:50%;transform:translateX(-50%);");
    mk(leftP, "left:2px;top:50%;transform:translateY(-50%);");
    mk(rightP, "right:2px;top:50%;transform:translateY(-50%);");
    return layer;
  }

  // 盒模型 hover → 在实际元素上高亮对应区域（Chrome F12 式）
  var boxRegionEl = null;
  function removeBoxRegion() {
    if (boxRegionEl && boxRegionEl.parentNode) boxRegionEl.parentNode.removeChild(boxRegionEl);
    boxRegionEl = null;
  }
  function showBoxRegion(region) {
    removeBoxRegion();
    var el = currentTarget();
    if (!el) return;
    var cs = window.getComputedStyle(el);
    var r = el.getBoundingClientRect(); // border box
    var n = function (p) { return parseFloat(cs.getPropertyValue(p)) || 0; };
    var mt = n("margin-top"), mr = n("margin-right"), mb = n("margin-bottom"), ml = n("margin-left");
    var bt = n("border-top-width"), br = n("border-right-width"), bb = n("border-bottom-width"), bl = n("border-left-width");
    var pt = n("padding-top"), pr = n("padding-right"), pb = n("padding-bottom"), pl = n("padding-left");
    var box;
    if (region === "margin") box = { left: r.left - ml, top: r.top - mt, width: r.width + ml + mr, height: r.height + mt + mb, color: "rgba(246,178,107,0.55)" };
    else if (region === "border") box = { left: r.left, top: r.top, width: r.width, height: r.height, color: "rgba(253,201,108,0.6)" };
    else if (region === "padding") box = { left: r.left + bl, top: r.top + bt, width: r.width - bl - br, height: r.height - bt - bb, color: "rgba(147,196,125,0.55)" };
    else box = { left: r.left + bl + pl, top: r.top + bt + pt, width: r.width - bl - br - pl - pr, height: r.height - bt - bb - pt - pb, color: "rgba(111,168,220,0.55)" };
    var d = document.createElement("div");
    d.setAttribute(OVERLAY_ATTR, "region");
    d.style.cssText = "position:fixed;pointer-events:none;z-index:2147483645;left:" + box.left + "px;top:" + box.top +
      "px;width:" + Math.max(0, box.width) + "px;height:" + Math.max(0, box.height) + "px;background:" + box.color + ";";
    document.documentElement.appendChild(d);
    boxRegionEl = d;
  }

  function buildBoxModel() {
    var wrap = document.createElement("div");
    wrap.style.cssText = "padding:10px;display:flex;justify-content:center;background:#fafbfc;border-bottom:1px solid #eaedf0;";
    var el = currentTarget();
    if (!el) return wrap;
    var cs = window.getComputedStyle(el);
    var margin = boxLayer("#f7cd9c", "margin", "margin-top", "margin-right", "margin-bottom", "margin-left");
    var border = boxLayer("#fdd9a0", "border", "border-top-width", "border-right-width", "border-bottom-width", "border-left-width");
    var padding = boxLayer("#c3dca4", "padding", "padding-top", "padding-right", "padding-bottom", "padding-left");
    var content = document.createElement("div");
    content.setAttribute("data-box-region", "content");
    content.style.cssText = "background:#a3c5e8;border-radius:2px;padding:6px 14px;font:11px ui-monospace,monospace;color:#1f2328;white-space:nowrap;";
    var w = Math.round(parseFloat(cs.width) || 0), h = Math.round(parseFloat(cs.height) || 0);
    content.textContent = w + " × " + h;
    padding.appendChild(content);
    border.appendChild(padding);
    margin.appendChild(border);
    wrap.appendChild(margin);
    // hover 盒模型某层 → 高亮元素对应区域（取最内层有 data-box-region 的祖先）
    wrap.addEventListener("mouseover", function (e) {
      var node = e.target;
      while (node && node !== wrap) {
        var reg = node.getAttribute && node.getAttribute("data-box-region");
        if (reg) { showBoxRegion(reg); return; }
        node = node.parentElement;
      }
    });
    wrap.addEventListener("mouseleave", removeBoxRegion);
    return wrap;
  }

  // ── 全部 CSS 列表（computed 全量，搜索 + 每条可改立即生效）──
  function buildCssList() {
    var wrap = document.createElement("div");
    wrap.style.cssText = "border-top:1px solid #eaedf0;";
    var head = document.createElement("div");
    head.style.cssText = "display:flex;align-items:center;gap:6px;padding:7px 10px;cursor:pointer;user-select:none;font-size:12px;color:#1f2328;";
    var chev = document.createElement("span"); chev.textContent = "▸"; chev.style.cssText = "color:#8c949e;font-size:10px;";
    var title = document.createElement("span"); title.textContent = "全部 CSS"; title.style.cssText = "flex:1;font-weight:500;";
    head.appendChild(chev); head.appendChild(title);
    var body = document.createElement("div"); body.style.cssText = "display:none;flex-direction:column;";
    var search = document.createElement("input");
    search.type = "text"; search.placeholder = "搜索属性…";
    search.style.cssText = "margin:6px 10px;height:24px;background:#f6f8fa;color:#1f2328;border:1px solid #d9dde3;border-radius:6px;font-size:12px;padding:0 8px;outline:none;";
    var list = document.createElement("div"); list.style.cssText = "padding:0 10px 8px;";
    body.appendChild(search); body.appendChild(list);
    var built = false;
    var buildRows = function () {
      if (built) return; built = true;
      var el = currentTarget();
      if (!el) return;
      var cs = window.getComputedStyle(el);
      var frag = document.createDocumentFragment();
      var names = [];
      for (var i = 0; i < cs.length; i++) names.push(cs[i]);
      names.sort();
      for (var j = 0; j < names.length; j++) frag.appendChild(cssRow(names[j], cs.getPropertyValue(names[j])));
      list.appendChild(frag);
    };
    search.addEventListener("input", function () {
      var q = search.value.trim().toLowerCase();
      var rows = list.children;
      for (var i = 0; i < rows.length; i++) {
        rows[i].style.display = !q || rows[i].getAttribute("data-prop").indexOf(q) >= 0 ? "flex" : "none";
      }
    });
    var collapsed = true;
    head.addEventListener("click", function () {
      collapsed = !collapsed;
      body.style.display = collapsed ? "none" : "flex";
      chev.textContent = collapsed ? "▸" : "▾";
      if (!collapsed) buildRows();
    });
    wrap.appendChild(head); wrap.appendChild(body);
    return wrap;
  }

  function cssRow(prop, value) {
    var row = document.createElement("div");
    row.setAttribute("data-prop", prop);
    row.style.cssText = "display:flex;align-items:center;gap:6px;padding:2px 0;font:11px ui-monospace,monospace;";
    var name = document.createElement("span");
    name.textContent = prop; name.title = prop;
    name.style.cssText = "flex:0 0 44%;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:#8250df;";
    var inp = document.createElement("input");
    inp.type = "text"; inp.value = value;
    inp.style.cssText = "flex:1;min-width:0;height:20px;background:transparent;color:#1f2328;border:1px solid transparent;border-radius:4px;font:11px ui-monospace,monospace;padding:0 4px;outline:none;";
    inp.addEventListener("focus", function () { inp.style.background = "#f6f8fa"; inp.style.borderColor = "#d9dde3"; });
    inp.addEventListener("blur", function () { inp.style.background = "transparent"; inp.style.borderColor = "transparent"; });
    inp.addEventListener("input", function () { styleApplyAny(prop, inp.value); });
    row.appendChild(name); row.appendChild(inp);
    return row;
  }

  function showAnnotationCard(snap) {
    removeCard();
    cardSnapshot = snap;
    var card = document.createElement("div");
    card.setAttribute(OVERLAY_ATTR, "card");
    var cardTop = 16;
    card.style.cssText = [
      "position:fixed", "top:" + cardTop + "px", "right:16px", "width:300px", "max-height:84vh",
      "display:flex", "flex-direction:column", "z-index:2147483647",
      "background:#ffffff", "color:#1f2328", "border:1px solid #d9dde3",
      "border-radius:10px", "box-shadow:0 8px 30px rgba(15,23,42,0.16)",
      "font-family:-apple-system,system-ui,sans-serif", "overflow:hidden",
    ].join(";");
    // 卡片内的点击/输入不冒泡到页面（冒泡阶段拦截——按钮自己的 handler 先触发，
    // 再在这里阻止继续冒泡到页面 document；不能用捕获阶段，否则会先于按钮 stopPropagation 把点击吃掉）
    card.addEventListener("click", function (e) { e.stopPropagation(); }, false);
    card.addEventListener("mousedown", function (e) { e.stopPropagation(); }, false);

    var head = document.createElement("div");
    head.style.cssText = "display:flex;align-items:center;gap:8px;padding:8px 10px;border-bottom:1px solid #d9dde3;cursor:move;user-select:none;";
    var badge = document.createElement("span");
    badge.textContent = elementBadge(snap);
    badge.style.cssText = "flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font:12px ui-monospace,monospace;";
    var closeBtn = document.createElement("button");
    closeBtn.textContent = "×";
    closeBtn.style.cssText = "border:none;background:none;color:#57606a;font-size:18px;line-height:1;cursor:pointer;padding:0 2px;";
    closeBtn.addEventListener("click", function () { styleRevert(); removeCard(); });
    head.appendChild(badge); head.appendChild(closeBtn);
    // 拖动：按住头部移动卡片（改 left/top，避开 right 定位），避免遮住元素
    makeCardDraggable(card, head);

    var elementKey = elementKeyOf(selectedTarget);

    // ══ 子卡片 1：样式参数（可折叠）══
    var styleCard = document.createElement("div");
    styleCard.style.cssText = "border-bottom:1px solid #d9dde3;display:flex;flex-direction:column;min-height:0;";
    var styleHead = document.createElement("div");
    styleHead.style.cssText = "display:flex;align-items:center;gap:6px;padding:7px 10px;cursor:pointer;user-select:none;font-size:12px;color:#1f2328;";
    var chevron = document.createElement("span");
    chevron.textContent = "▾";
    chevron.style.cssText = "color:#8c949e;font-size:10px;";
    var styleTitle = document.createElement("span");
    styleTitle.textContent = "样式参数";
    styleTitle.style.cssText = "flex:1;font-weight:500;";
    styleHead.appendChild(chevron); styleHead.appendChild(styleTitle);
    var styleBody = document.createElement("div");
    styleBody.style.cssText = "display:flex;flex-direction:column;min-height:0;max-height:52vh;overflow-y:auto;";
    var boxModel = buildBoxModel(); // Chrome F12 式盒模型图
    var fields = document.createElement("div");
    fields.style.cssText = "padding:8px 10px;";
    for (var i = 0; i < CARD_FIELDS.length; i++) fields.appendChild(cardRow(CARD_FIELDS[i]));
    var cssList = buildCssList(); // 全部 CSS（折叠）
    var pushStyleToAside = null; // chatCard 构造后赋值——把当前样式改动发到下面的临时对话
    var styleFoot = document.createElement("div");
    styleFoot.style.cssText = "display:flex;justify-content:flex-end;gap:8px;padding:6px 10px 8px;";
    var sCancel = mkFlatBtn("撤销"); sCancel.addEventListener("click", function () { styleRevert(); });
    var sAside = mkFlatBtn("到临时对话");
    sAside.title = "把刚调的样式改动发到下面的临时对话，继续和助手讨论 / 让它定位源码";
    sAside.addEventListener("click", function () {
      var diff = takeStyleDiff();
      if (!diff.length) return;
      var txt = "我在样式参数里手动调了这些：\n" +
        diff.map(function (d) { return "· " + d.prop + ": " + d.before + " → " + d.after; }).join("\n") +
        "\n请基于此继续——告诉我这些改动对应源码该怎么改，或我们再调调。";
      if (pushStyleToAside) pushStyleToAside(txt);
    });
    var sSend = mkPrimaryBtn("加入队列");
    sSend.title = "把这个元素的改动加入修改队列；攒够多个元素后在队列框里统一提交到主对话";
    sSend.addEventListener("click", function () {
      addToQueue(elementKey, elementBadge(cardSnapshot), cardSnapshot, takeStyleDiff());
      removeCard();
    });
    styleFoot.appendChild(sCancel); styleFoot.appendChild(sAside); styleFoot.appendChild(sSend);
    styleBody.appendChild(boxModel); styleBody.appendChild(fields); styleBody.appendChild(cssList); styleBody.appendChild(styleFoot);
    var styleCollapsed = false;
    styleHead.addEventListener("click", function () {
      styleCollapsed = !styleCollapsed;
      styleBody.style.display = styleCollapsed ? "none" : "flex";
      chevron.textContent = styleCollapsed ? "▸" : "▾";
    });
    styleCard.appendChild(styleHead); styleCard.appendChild(styleBody);

    // ══ 子卡片 2：和助手一起改（LLM 对话面板 + 模型选择器）══
    var chatCard = document.createElement("div");
    chatCard.style.cssText = "display:flex;flex-direction:column;min-height:0;flex:1;";
    var chatHead = document.createElement("div");
    chatHead.style.cssText = "display:flex;align-items:center;gap:6px;padding:7px 10px;font-size:12px;color:#1f2328;";
    var chatTitle = document.createElement("span");
    chatTitle.textContent = "和助手一起改";
    chatTitle.style.cssText = "flex:none;font-weight:500;";
    var modelSelect = document.createElement("select");
    modelSelect.style.cssText = "flex:1;min-width:0;height:24px;background:#f6f8fa;color:#1f2328;border:1px solid #d9dde3;border-radius:6px;font-size:11px;padding:0 4px;";
    var optLoading = document.createElement("option"); optLoading.textContent = "默认模型"; modelSelect.appendChild(optLoading);
    chatHead.appendChild(chatTitle); chatHead.appendChild(modelSelect);
    var msgList = document.createElement("div");
    msgList.style.cssText = "display:flex;flex-direction:column;gap:6px;padding:6px 10px;overflow-y:auto;flex:1;min-height:140px;";
    var chatInputRow = document.createElement("div");
    chatInputRow.style.cssText = "display:flex;gap:6px;padding:6px 10px;border-top:1px solid #d9dde3;align-items:flex-end;";
    var chatInput = document.createElement("textarea");
    chatInput.placeholder = "让它改这个元素，比如「圆角大一点、配色柔和些」（⌘↵ 发送）";
    chatInput.rows = 2;
    chatInput.style.cssText = "flex:1;resize:none;background:#f6f8fa;color:#1f2328;border:1px solid #d9dde3;border-radius:6px;font-size:12px;padding:6px;outline:none;";
    var chatSend = mkPrimaryBtn("发送");
    var sendChat = function () {
      var t = chatInput.value.trim();
      if (!t) return;
      chatInput.value = "";
      appendChatMsg(msgList, "user", t);
      asideConvos[elementKey] = asideConvos[elementKey] || { sessionId: null, messages: [] };
      asideConvos[elementKey].messages.push({ role: "user", text: t });
      cardChat.assistantRow = null;
      var sel = modelSelect.value ? modelSelect.value.split("|") : ["", ""];
      send("heb:aside:send", {
        surface: window.__HEB_POPOUT__ ? "popout" : "embedded",
        elementKey: elementKey,
        sessionId: asideConvos[elementKey].sessionId,
        text: t,
        providerId: sel[0] || undefined,
        model: sel[1] || undefined,
        element: elementLocator(cardSnapshot),
      });
    };
    chatSend.addEventListener("click", sendChat);
    chatInput.addEventListener("keydown", function (e) { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); sendChat(); } });
    chatInputRow.appendChild(chatInput); chatInputRow.appendChild(chatSend);
    // 样式参数区的「到临时对话」按钮回调：发到旁支会话 + 折叠样式区露出对话
    pushStyleToAside = function (text) {
      chatInput.value = text;
      sendChat();
      styleCollapsed = true; styleBody.style.display = "none"; chevron.textContent = "▸";
    };
    var chatFoot = document.createElement("div");
    chatFoot.style.cssText = "display:flex;justify-content:flex-end;padding:6px 10px;border-top:1px solid #d9dde3;";
    var submitMain = mkPrimaryBtn("提交到主对话");
    submitMain.addEventListener("click", function () {
      var conv = asideConvos[elementKey];
      if (!conv || !conv.sessionId) { appendChatMsg(msgList, "assistant", "（还没开始对话）"); return; }
      appendChatMsg(msgList, "assistant", "正在总结并提交到主对话…");
      send("heb:aside:submit", { surface: window.__HEB_POPOUT__ ? "popout" : "embedded", sessionId: conv.sessionId, element: elementLocator(cardSnapshot) });
    });
    chatFoot.appendChild(submitMain);
    chatCard.appendChild(chatHead); chatCard.appendChild(msgList); chatCard.appendChild(chatInputRow); chatCard.appendChild(chatFoot);

    cardChat = { elementKey: elementKey, sessionId: (asideConvos[elementKey] && asideConvos[elementKey].sessionId) || null, msgList: msgList, assistantRow: null, modelSelect: modelSelect };
    if (asideConvos[elementKey]) {
      for (var m = 0; m < asideConvos[elementKey].messages.length; m++) {
        appendChatMsg(msgList, asideConvos[elementKey].messages[m].role, asideConvos[elementKey].messages[m].text);
      }
    }
    // 请求模型列表填充选择器
    send("heb:aside:models:request", { surface: window.__HEB_POPOUT__ ? "popout" : "embedded" });

    card.appendChild(head);
    card.appendChild(styleCard);
    card.appendChild(chatCard);
    document.documentElement.appendChild(card);
    cardEl = card;
  }

  function fillModelSelect(select, list, current) {
    if (!select) return;
    select.innerHTML = "";
    var arr = Array.isArray(list) ? list : [];
    if (!arr.length && current && current.model) arr = [{ providerId: current.providerId, model: current.model, label: current.model }];
    for (var i = 0; i < arr.length; i++) {
      var o = document.createElement("option");
      o.value = (arr[i].providerId || "") + "|" + (arr[i].model || "");
      o.textContent = arr[i].label || arr[i].model || "?";
      if (current && arr[i].providerId === current.providerId && arr[i].model === current.model) o.selected = true;
      select.appendChild(o);
    }
  }

  function mkFlatBtn(label) {
    var b = document.createElement("button");
    b.textContent = label;
    b.style.cssText = "padding:4px 10px;font-size:12px;background:none;border:none;color:#57606a;cursor:pointer;";
    return b;
  }
  function mkPrimaryBtn(label) {
    var b = document.createElement("button");
    b.textContent = label;
    b.style.cssText = "padding:4px 12px;font-size:12px;background:#2f81f7;border:none;color:#fff;border-radius:6px;cursor:pointer;";
    return b;
  }

  function appendChatMsg(msgList, role, text) {
    var row = document.createElement("div");
    if (role === "user") {
      row.style.cssText = "align-self:flex-end;max-width:85%;background:#2f81f7;color:#fff;border-radius:10px;padding:6px 9px;font-size:12px;white-space:pre-wrap;word-break:break-word;";
    } else if (role === "tool") {
      row.style.cssText = "align-self:flex-start;max-width:90%;background:#eafaf0;color:#1a7f4b;border-radius:8px;padding:4px 8px;font:11px ui-monospace,monospace;";
    } else {
      row.style.cssText = "align-self:flex-start;max-width:90%;background:#f1f3f5;color:#1f2328;border-radius:10px;padding:6px 9px;font-size:12px;white-space:pre-wrap;word-break:break-word;";
    }
    row.textContent = text;
    msgList.appendChild(row);
    msgList.scrollTop = msgList.scrollHeight;
    return row;
  }

  /* ───────────────────────── picker 状态机 ───────────────────────── */

  var pickerActive = false;

  function isOurNode(node) {
    var el = node;
    var guard = 0;
    while (el && guard < 6) {
      if (el.getAttribute && el.getAttribute(OVERLAY_ATTR) !== null) return true;
      el = el.parentElement;
      guard += 1;
    }
    return false;
  }

  function pickableAt(x, y) {
    var el = document.elementFromPoint(x, y);
    if (!el || isOurNode(el)) return null;
    if (el === document.documentElement || el === document.body) return null;
    return el;
  }

  var lastHitTest = 0;
  var lastMouse = { x: 0, y: 0 };
  var wheelLockUntil = 0; // 滚轮调整父子后短暂锁定，避免 mousemove 抖动重置
  function onMouseMove(e) {
    if (!pickerActive) return;
    lastMouse.x = e.clientX;
    lastMouse.y = e.clientY;
    if (Date.now() < wheelLockUntil) return; // 滚轮锁定期内不跟随鼠标
    var now = Date.now();
    if (now - lastHitTest < 33) return; // 节流 ~30fps（复杂页面每像素跑会卡）
    lastHitTest = now;
    hoverTarget = pickableAt(e.clientX, e.clientY);
  }

  // 找 parent 里包含 (x,y) 的直接子元素（滚轮下钻用）；没命中点则取第一个非 overlay 子
  function childAtPoint(parent, x, y) {
    var fallback = null;
    for (var i = 0; i < parent.children.length; i++) {
      var c = parent.children[i];
      if (isOurNode(c)) continue;
      if (!fallback) fallback = c;
      var r = c.getBoundingClientRect();
      if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) return c;
    }
    return fallback;
  }

  // 滚轮选父/子（DevTools / 截图工具式精确选取）：上滚→父，下滚→子
  function onWheel(e) {
    if (!pickerActive) return;
    e.preventDefault();
    e.stopPropagation();
    if (!hoverTarget) hoverTarget = pickableAt(lastMouse.x, lastMouse.y);
    if (!hoverTarget) return;
    if (e.deltaY < 0) {
      var p = hoverTarget.parentElement;
      if (p && p !== document.body && p !== document.documentElement && !isOurNode(p)) hoverTarget = p;
    } else if (e.deltaY > 0) {
      var child = childAtPoint(hoverTarget, lastMouse.x, lastMouse.y);
      if (child) hoverTarget = child;
    }
    wheelLockUntil = Date.now() + 700;
  }

  function onClick(e) {
    if (!pickerActive) return;
    e.preventDefault();
    e.stopPropagation();
    // 优先用当前 hover（可能被滚轮选成了父/子），否则回退到鼠标点下的元素
    var el = (hoverTarget && hoverTarget.isConnected) ? hoverTarget : pickableAt(e.clientX, e.clientY);
    if (!el || isOurNode(el)) return;
    selectedTarget = el;
    hoverTarget = null;
    styleDiff = {};
    flashSelect(el); // 点中瞬间闪一下，给「按下选中」的反馈
    stopPicker(false);
    // 通知宿主 picker 已结束（选中成功也算结束）→ embedded 模式 React 按钮恢复非激活态
    send("heb:picker:cancelled", {});
    showAnnotationCard(collectSnapshot(el));
  }

  // 选中瞬间在元素上叠一层蓝色高亮快速淡出——告诉用户「点中了」
  function flashSelect(el) {
    try {
      var r = el.getBoundingClientRect();
      var f = document.createElement("div");
      f.setAttribute(OVERLAY_ATTR, "flash");
      f.style.cssText = "position:fixed;pointer-events:none;z-index:2147483646;border-radius:3px;" +
        "left:" + r.left + "px;top:" + r.top + "px;width:" + r.width + "px;height:" + r.height + "px;" +
        "background:rgba(47,129,247,0.35);transition:opacity .35s ease;opacity:1;";
      document.documentElement.appendChild(f);
      requestAnimationFrame(function () { f.style.opacity = "0"; });
      setTimeout(function () { if (f.parentNode) f.parentNode.removeChild(f); }, 400);
    } catch (e) {}
  }

  function onKeyDown(e) {
    if (!pickerActive) return;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      stopPicker(true);
    }
  }

  function startPicker() {
    if (pickerActive) return;
    pickerActive = true;
    ensureOverlayLoop();
    document.addEventListener("mousemove", onMouseMove, true);
    document.addEventListener("click", onClick, true);
    document.addEventListener("mousedown", swallow, true);
    document.addEventListener("mouseup", swallow, true);
    document.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("wheel", onWheel, { capture: true, passive: false });
  }

  function swallow(e) {
    if (pickerActive) {
      e.preventDefault();
      e.stopPropagation();
    }
  }

  function stopPicker(cancelled) {
    if (!pickerActive) return;
    pickerActive = false;
    hoverTarget = null;
    document.removeEventListener("mousemove", onMouseMove, true);
    document.removeEventListener("click", onClick, true);
    document.removeEventListener("mousedown", swallow, true);
    document.removeEventListener("mouseup", swallow, true);
    document.removeEventListener("keydown", onKeyDown, true);
    document.removeEventListener("wheel", onWheel, { capture: true });
    if (cancelled) send("heb:picker:cancelled", {});
  }

  function clearSelection() {
    removeCard();
    selectedTarget = null;
    styleDiff = {};
  }

  /* ───────────────────────── SPA 导航上报 ─────────────────────────
     popout 工具栏现在是独立 webview（不再注入页面），导航态由 Rust 收到 heb:navigated
     后 eval 工具栏更新；embedded 则由主窗口 React 收 browser:// 事件。 */

  function reportNavigated() {
    send("heb:navigated", { url: window.location.href, title: document.title || "" });
  }

  function hookHistory() {
    try {
      var origPush = window.history.pushState;
      var origReplace = window.history.replaceState;
      window.history.pushState = function () {
        var r = origPush.apply(this, arguments);
        reportNavigated();
        return r;
      };
      window.history.replaceState = function () {
        var r = origReplace.apply(this, arguments);
        reportNavigated();
        return r;
      };
      window.addEventListener("popstate", reportNavigated);
      window.addEventListener("hashchange", reportNavigated);
    } catch (e) {
      /* 静默 */
    }
  }

  /* ───────────────────────── 入向消息分发 ───────────────────────── */

  function handleIn(raw) {
    var msg = parseInMsg(raw);
    if (!msg) return;
    switch (msg.type) {
      case "heb:picker:start":
        startPicker();
        break;
      case "heb:picker:stop":
        stopPicker(false);
        break;
      case "heb:selection:clear":
        clearSelection();
        break;
      case "heb:style:apply":
        if (msg.payload) styleApply(msg.payload.prop, msg.payload.value);
        break;
      case "heb:style:revert":
        styleRevert();
        break;
      case "heb:style:take-diff":
        send("heb:style:diff", { diff: takeStyleDiff() });
        break;
      case "heb:overlay:hide":
        overlaysHidden = true;
        break;
      case "heb:overlay:show":
        overlaysHidden = false;
        break;
      // ── 元素对话（旁支会话）下行 ──
      case "heb:aside:session":
        if (msg.payload) {
          var ek = msg.payload.elementKey;
          asideConvos[ek] = asideConvos[ek] || { sessionId: null, messages: [] };
          asideConvos[ek].sessionId = msg.payload.sessionId;
          if (cardChat && cardChat.elementKey === ek) cardChat.sessionId = msg.payload.sessionId;
        }
        break;
      case "heb:aside:models":
        if (cardChat && msg.payload) fillModelSelect(cardChat.modelSelect, msg.payload.list, msg.payload.current);
        break;
      case "heb:aside:delta":
        if (cardChat && msg.payload) {
          if (!cardChat.assistantRow) cardChat.assistantRow = appendChatMsg(cardChat.msgList, "assistant", "");
          cardChat.assistantRow.textContent += msg.payload.text || "";
          cardChat.msgList.scrollTop = cardChat.msgList.scrollHeight;
        }
        break;
      case "heb:aside:apply":
        if (msg.payload) {
          styleApply(msg.payload.prop, msg.payload.value); // 实时改页面
          if (cardChat) appendChatMsg(cardChat.msgList, "tool", "🎨 " + msg.payload.prop + " → " + msg.payload.value);
        }
        break;
      case "heb:aside:done":
        if (cardChat && cardChat.assistantRow) {
          var conv = asideConvos[cardChat.elementKey];
          if (conv) conv.messages.push({ role: "assistant", text: cardChat.assistantRow.textContent });
          cardChat.assistantRow = null;
        }
        break;
      case "heb:aside:submitted":
        if (cardChat) appendChatMsg(cardChat.msgList, "assistant", "✅ 已提交到主对话，主对话会据此改源码");
        break;
      case "heb:aside:error":
        if (cardChat && msg.payload) appendChatMsg(cardChat.msgList, "assistant", "⚠️ " + (msg.payload.message || "出错了"));
        break;
      default:
        break;
    }
  }

  // 下行入口：wry 模式 Rust eval 调它；iframe 模式 message 事件喂它。
  window.__HEB_RX__ = handleIn;
  if (IN_IFRAME) {
    window.addEventListener("message", function (e) {
      handleIn(e.data);
    });
  }

  /* ───────────────────────── boot ───────────────────────── */

  function boot() {
    hookHistory();
    send("heb:ready", { url: window.location.href, title: document.title || "" });
  }

  if (document.readyState !== "loading") {
    setTimeout(boot, 0);
  } else {
    document.addEventListener("DOMContentLoaded", boot);
  }
})();
