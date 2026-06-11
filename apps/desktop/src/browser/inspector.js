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
  function capSnapshot(snap) {
    var droppable = ["childrenSummary", "computedStyles", "attributes", "react"];
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
    }
    var children = [];
    for (var k = 0; k < el.children.length && k < 10; k++) {
      children.push(el.children[k].tagName.toLowerCase());
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
      innerText: truncate(el.innerText || "", 500),
      react: react,
      boundingClientRect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
      computedStyles: computed,
      parent: el.parentElement
        ? {
            tagName: el.parentElement.tagName.toLowerCase(),
            classList: Array.prototype.slice.call(el.parentElement.classList || [], 0, 5),
          }
        : undefined,
      childrenSummary: children,
    };
    return capSnapshot(snap);
  }

  /* ───────────────────────── styler ───────────────────────── */

  var styleDiff = {}; // prop -> { before, after }

  function styleApply(prop, value) {
    if (!selectedTarget || STYLE_WHITELIST.indexOf(prop) === -1) return;
    try {
      if (!(prop in styleDiff)) {
        styleDiff[prop] = { before: selectedTarget.style.getPropertyValue(prop), after: value };
      } else {
        styleDiff[prop].after = value;
      }
      selectedTarget.style.setProperty(prop, value);
    } catch (e) {
      /* 静默 */
    }
  }

  function styleRevert() {
    if (!selectedTarget) {
      styleDiff = {};
      return;
    }
    try {
      var props = Object.keys(styleDiff);
      for (var i = props.length - 1; i >= 0; i--) {
        var prop = props[i];
        var before = styleDiff[prop].before;
        if (before) selectedTarget.style.setProperty(prop, before);
        else selectedTarget.style.removeProperty(prop);
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

  // 样式编辑器字段（对齐用户截图：字号/字重/颜色/圆角/边框/间距）
  var CARD_FIELDS = [
    { prop: "font-size", label: "字号", kind: "px" },
    { prop: "font-weight", label: "字重", kind: "select", options: ["300", "400", "500", "600", "700", "800"] },
    { prop: "color", label: "文字颜色", kind: "color" },
    { prop: "text-align", label: "对齐", kind: "select", options: ["left", "center", "right", "justify"] },
    { prop: "background-color", label: "背景色", kind: "color" },
    { prop: "border-radius", label: "圆角", kind: "px" },
    { prop: "border-width", label: "边框宽度", kind: "px" },
    { prop: "border-color", label: "边框颜色", kind: "color" },
    { prop: "padding", label: "内边距", kind: "px" },
    { prop: "margin", label: "外边距", kind: "px" },
  ];

  function readComputed(prop) {
    try {
      var v = window.getComputedStyle(selectedTarget).getPropertyValue(prop);
      if ((!v || v === "") && prop === "border-width") v = window.getComputedStyle(selectedTarget).getPropertyValue("border-top-width");
      if ((!v || v === "") && prop === "border-color") v = window.getComputedStyle(selectedTarget).getPropertyValue("border-top-color");
      if ((!v || v === "") && prop === "padding") v = window.getComputedStyle(selectedTarget).getPropertyValue("padding-top");
      if ((!v || v === "") && prop === "margin") v = window.getComputedStyle(selectedTarget).getPropertyValue("margin-top");
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

  function elementBadge(snap) {
    var t = snap.tagName;
    if (snap.id) t += "#" + snap.id;
    else if (snap.classList && snap.classList.length) t += "." + snap.classList[0];
    var comp = snap.react && snap.react.componentChain && snap.react.componentChain[0];
    return comp ? t + "  ⟨" + comp + "⟩" : t;
  }

  function showAnnotationCard(snap) {
    removeCard();
    cardSnapshot = snap;
    var card = document.createElement("div");
    card.setAttribute(OVERLAY_ATTR, "card");
    var cardTop = window.__HEB_POPOUT__ ? TOOLBAR_H + 12 : 16;
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

    var comment = document.createElement("textarea");
    comment.placeholder = "描述这些更改…"; // 描述这些更改…
    comment.rows = 2;
    comment.style.cssText = "margin:8px 10px 0;resize:none;background:#f6f8fa;color:#1f2328;border:1px solid #d9dde3;border-radius:6px;font-size:13px;padding:6px;outline:none;";

    var body = document.createElement("div");
    body.style.cssText = "padding:8px 10px;overflow-y:auto;flex:1;";
    var groupTitle = document.createElement("div");
    groupTitle.textContent = "样式调整"; // 样式调整
    groupTitle.style.cssText = "font-size:11px;color:#6e7681;margin-bottom:6px;";
    body.appendChild(groupTitle);
    for (var i = 0; i < CARD_FIELDS.length; i++) body.appendChild(cardRow(CARD_FIELDS[i]));

    var foot = document.createElement("div");
    foot.style.cssText = "display:flex;justify-content:flex-end;gap:8px;padding:8px 10px;border-top:1px solid #d9dde3;";
    var cancel = document.createElement("button");
    cancel.textContent = "取消";
    cancel.style.cssText = "padding:4px 10px;font-size:12px;background:none;border:none;color:#57606a;cursor:pointer;";
    cancel.addEventListener("click", function () { styleRevert(); removeCard(); });
    var submit = document.createElement("button");
    submit.textContent = "发送到对话";
    submit.style.cssText = "padding:4px 12px;font-size:12px;background:#2f81f7;border:none;color:#fff;border-radius:6px;cursor:pointer;";
    submit.addEventListener("click", function () {
      send("heb:annotation:submit", {
        snapshot: cardSnapshot,
        comment: comment.value || "",
        styleDiff: takeStyleDiff(),
      });
      removeCard();
    });
    foot.appendChild(cancel); foot.appendChild(submit);

    card.appendChild(head);
    card.appendChild(comment);
    card.appendChild(body);
    card.appendChild(foot);
    document.documentElement.appendChild(card);
    cardEl = card;
    setTimeout(function () { comment.focus(); }, 0);
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

  function onMouseMove(e) {
    if (!pickerActive) return;
    hoverTarget = pickableAt(e.clientX, e.clientY);
  }

  function onClick(e) {
    if (!pickerActive) return;
    e.preventDefault();
    e.stopPropagation();
    var el = pickableAt(e.clientX, e.clientY);
    if (!el) return;
    selectedTarget = el;
    hoverTarget = null;
    styleDiff = {};
    stopPicker(false);
    showAnnotationCard(collectSnapshot(el));
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
    if (cancelled) send("heb:picker:cancelled", {});
  }

  function clearSelection() {
    removeCard();
    selectedTarget = null;
    styleDiff = {};
  }

  /* ─────────────────── popout 窗口内工具栏（仅 __HEB_POPOUT__）───────────────────
     popout 直接加载目标页面（无我们的 React），工具栏由 inspector 在页面内渲染：
     地址栏 + 后退/前进/刷新 + 选取元素。导航走原生 window.location/history，
     Rust on_navigation 仍做两档安全校验。注释卡片复用同一套页面内卡片。 */

  var popoutAddr = null;
  var TOOLBAR_H = 40;

  function navWithScheme(raw) {
    var v = (raw || "").trim();
    if (!v) return;
    if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(v)) {
      var host = v.split("/")[0].split(":")[0].toLowerCase();
      var local = host === "localhost" || /\.localhost$/.test(host) || host === "host.docker.internal" ||
        /\.local$/.test(host) || /^127\./.test(host) || /^10\./.test(host) || /^192\.168\./.test(host) ||
        /^172\.(1[6-9]|2\d|3[01])\./.test(host) || host === "0.0.0.0";
      v = (local ? "http://" : "https://") + v;
    }
    window.location.href = v;
  }

  function popoutBtn(label, title, onClick) {
    var b = document.createElement("button");
    b.textContent = label;
    b.title = title;
    b.style.cssText = "flex:none;width:28px;height:28px;border:1px solid #d9dde3;background:#fff;color:#1f2328;" +
      "border-radius:6px;cursor:pointer;font-size:14px;line-height:1;display:flex;align-items:center;justify-content:center;";
    b.addEventListener("click", function (e) { e.preventDefault(); e.stopPropagation(); onClick(); });
    return b;
  }

  function showPopoutToolbar() {
    var bar = document.createElement("div");
    bar.setAttribute(OVERLAY_ATTR, "toolbar");
    bar.style.cssText = [
      "position:fixed", "top:0", "left:0", "right:0", "height:" + TOOLBAR_H + "px",
      "display:flex", "align-items:center", "gap:6px", "padding:0 8px", "box-sizing:border-box",
      "background:#f6f8fa", "border-bottom:1px solid #d9dde3", "z-index:2147483646",
      "font-family:-apple-system,system-ui,sans-serif",
    ].join(";");
    bar.addEventListener("click", function (e) { e.stopPropagation(); }, false);
    bar.addEventListener("mousedown", function (e) { e.stopPropagation(); }, false);

    bar.appendChild(popoutBtn("‹", "后退", function () { history.back(); }));
    bar.appendChild(popoutBtn("›", "前进", function () { history.forward(); }));
    bar.appendChild(popoutBtn("⟳", "刷新", function () { location.reload(); }));

    var addr = document.createElement("input");
    addr.type = "text";
    addr.value = window.location.href;
    addr.spellcheck = false;
    addr.style.cssText = "flex:1;height:28px;border:1px solid #d9dde3;background:#fff;color:#1f2328;" +
      "border-radius:14px;padding:0 12px;font-size:12px;outline:none;box-sizing:border-box;";
    addr.addEventListener("keydown", function (e) { if (e.key === "Enter") { e.preventDefault(); navWithScheme(addr.value); } });
    bar.appendChild(addr);
    popoutAddr = addr;

    bar.appendChild(popoutBtn("⌖", "选取页面元素标注", function () {
      if (pickerActive) stopPicker(false); else startPicker();
    }));

    document.documentElement.appendChild(bar);
    // 把页面内容下移，避免被工具栏盖住
    try { document.body.style.marginTop = TOOLBAR_H + "px"; } catch (e) {}
  }

  /* ───────────────────────── SPA 导航上报 ───────────────────────── */

  function reportNavigated() {
    if (popoutAddr) popoutAddr.value = window.location.href;
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
    if (window.__HEB_POPOUT__) {
      try { showPopoutToolbar(); } catch (e) {}
    }
    send("heb:ready", { url: window.location.href, title: document.title || "" });
  }

  if (document.readyState !== "loading") {
    setTimeout(boot, 0);
  } else {
    document.addEventListener("DOMContentLoaded", boot);
  }
})();
