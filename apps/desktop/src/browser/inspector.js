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

  // "@2" -> 1（0-based index）；非法返回 -1。注释框 @N 引用解析用。
  function refToIndex(ref) {
    var m = /^@(\d+)$/.exec(String(ref || "").trim());
    if (!m) return -1;
    var n = parseInt(m[1], 10);
    return n >= 1 ? n - 1 : -1;
  }

  // contenteditable 输入框的子节点序列 → 发送给助手的纯文本。
  // nodes: [{type:"text",value} | {type:"ref",ref:"@2",locator}]。
  // chip 还原成「元素2: <locator>」让助手拿到元素定位。
  function composeAsideText(nodes) {
    var out = "";
    for (var i = 0; i < nodes.length; i++) {
      var n = nodes[i];
      if (n.type === "ref") {
        var idx = refToIndex(n.ref);
        out += "「元素" + (idx + 1) + (n.locator ? ": " + n.locator : "") + "」";
      } else {
        out += n.value || "";
      }
    }
    return out;
  }

  // 一条注释（draft）的旁支对话只有一个会话，恒定锚在 1 号元素上——切换激活元素
  // （看样式/改参数）不改变对话归属。返回 elements[0] 的 key 作为 asideConvos 索引。
  // 关键修复：曾经 chat 区用激活元素 key、syncDraftToList 用 elements[0] key，
  // 两套不一致 → 切到 2 号聊天后对话历史读不回、会话漂移。
  function draftChatKey(draft) {
    if (!draft || !draft.elements || !draft.elements.length) return null;
    return draft.elements[0].key;
  }

  // 在 draft 里找元素的下标：先比 DOM 引用（同一节点），再比 selectorPath
  // （React 重渲染换了 DOM 节点但还是同一逻辑元素 → 引用失配但 selector 相同）。
  // 找不到返回 -1。append 去重用它，避免同一逻辑元素被重复加入。
  function findDraftElementIndex(draft, el, snapshot) {
    if (!draft || !draft.elements) return -1;
    for (var i = 0; i < draft.elements.length; i++) {
      if (draft.elements[i].el === el) return i;
    }
    var sp = snapshot && snapshot.selectorPath;
    if (sp) {
      for (var j = 0; j < draft.elements.length; j++) {
        var es = draft.elements[j].snapshot;
        if (es && es.selectorPath && es.selectorPath === sp) return j;
      }
    }
    return -1;
  }

  // 样式改动块（对话流绿卡片）的可持久化定位信息：从 PreviewStyle 的 target 与作用
  // 元素算出一个 CSS selector，重建卡片时据此重新查 DOM 找回元素。
  // - selector 来源（target 非 @N）：直接用 target 本身 + allMatches
  // - @N 来源（单/多元素）：用该元素的 selectorPath 当 selector（allMatches=false）
  // 找不到 selectorPath 时返回 null —— 该改动无法重建定位，回填时降级为纯文本展示。
  function styleChangeLocate(target, allMatches, activeSnapshot) {
    var isRef = /^@\d+$/.test(String(target || "").trim());
    if (!isRef) return { selector: target, allMatches: !!allMatches };
    var sp = activeSnapshot && activeSnapshot.selectorPath;
    return sp ? { selector: sp, allMatches: false } : null;
  }

  // 按 locate 在 doc 里重新解析样式改动作用的元素集合（卡片重建时还原 change.els）。
  // doc 参数供单测注入；运行时传 document。解析失败/无匹配返回 []。
  function resolveStyleEls(locate, doc) {
    if (!locate || !locate.selector || !doc) return [];
    try {
      if (locate.allMatches) {
        return Array.prototype.slice.call(doc.querySelectorAll(locate.selector));
      }
      var one = doc.querySelector(locate.selector);
      return one ? [one] : [];
    } catch (e) {
      return [];
    }
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
    refToIndex: refToIndex,
    composeAsideText: composeAsideText,
    draftChatKey: draftChatKey,
    findDraftElementIndex: findDraftElementIndex,
    styleChangeLocate: styleChangeLocate,
    resolveStyleEls: resolveStyleEls,
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
  var SCROLL_ATTR = "data-hebbian-scroll";

  function ensureInspectorStyles() {
    if (document.getElementById("hebbian-inspector-style")) return;
    var style = document.createElement("style");
    style.id = "hebbian-inspector-style";
    style.textContent = "[" + SCROLL_ATTR + "]{scrollbar-width:thin;scrollbar-color:#c8d0d8 transparent;}" +
      "[" + SCROLL_ATTR + "]::-webkit-scrollbar{width:5px;height:5px;}" +
      "[" + SCROLL_ATTR + "]::-webkit-scrollbar-thumb{background:#c8d0d8;border-radius:999px;}" +
      "[" + SCROLL_ATTR + "]::-webkit-scrollbar-track{background:transparent;}";
    document.documentElement.appendChild(style);
  }

  function markScrollable(el) {
    ensureInspectorStyles();
    el.setAttribute(SCROLL_ATTR, "");
    return el;
  }

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

  function styleSet(prop, value, allowAny, src) {
    var el = currentTarget();
    if (!el) return;
    // CARD_FIELDS 走白名单（防误操作）；盒模型图 / 全部 CSS 列表走 allowAny（任意属性，
    // 改 inline style 不执行代码、安全）。
    if (!allowAny && STYLE_WHITELIST.indexOf(prop) === -1) return;
    try {
      if (!(prop in styleDiff)) {
        styleDiff[prop] = { before: el.style.getPropertyValue(prop), after: value, src: src || "css" };
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
    syncDraftToList();
  }
  // src 标记改动来源：fields=样式参数区 / css=盒模型+全部CSS——两区各自的「重置」只还原自己的
  function styleApply(prop, value) { styleSet(prop, value, false, "fields"); }
  function styleApplyAny(prop, value) { styleSet(prop, value, true, "css"); }

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
    syncDraftToList();
  }

  // 只还原指定来源（fields/css）的改动；激活元素生效。还原后同步注释列表。
  function styleRevertSrc(src) {
    var el = currentTarget();
    var props = Object.keys(styleDiff);
    for (var i = props.length - 1; i >= 0; i--) {
      var prop = props[i];
      if ((styleDiff[prop].src || "css") !== src) continue;
      try {
        var before = styleDiff[prop].before;
        if (el) {
          if (before) el.style.setProperty(prop, before);
          else el.style.removeProperty(prop);
        }
      } catch (e) { /* 静默 */ }
      delete styleDiff[prop];
    }
    syncDraftToList();
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
  var cardPos = null; // 用户拖动后记住的位置 {x,y}；非空则不再自动避让
  var cardSnapshot = null;
  // 多元素注释 draft：一个注释框选中 N 个元素（@1=elements[0]）。
  // selectedTarget/cardSnapshot/styleDiff 三个旧全局是"激活元素"的视图——
  // setActiveElement 切换指向，currentTarget/styleSet/盒模型等存量代码零改动。
  var draft = null; // { elements: [{key, el, snapshot, styleDiff}], activeIndex, structuralChanges: [] }

  function newDraft(el, snapshot) {
    return {
      elements: [{ key: elementKeyOf(el), el: el, snapshot: snapshot, styleDiff: {} }],
      activeIndex: 0,
      structuralChanges: [],
    };
  }

  // 把旧全局视图指到 draft.elements[i]（styleDiff 引用同一对象，改动直接落到该元素）
  function setActiveElement(i) {
    if (!draft || i < 0 || i >= draft.elements.length) return;
    draft.activeIndex = i;
    var item = draft.elements[i];
    selectedTarget = item.el;
    cardSnapshot = item.snapshot;
    styleDiff = item.styleDiff;
  }

  // @N → 元素（detach 时用 snapshot 的 selector/xpath 找回）；
  // 非 @N 格式当 CSS selector 直接在页面上找（模型可触达未圈选元素）；
  // 非法/越界回退激活元素
  function elementForRef(ref) {
    var raw = String(ref || "").trim();
    if (raw && !/^@\d+$/.test(raw)) {
      try { var bySel = document.querySelector(raw); if (bySel) return bySel; } catch (e) {}
    }
    if (!draft) return currentTarget();
    var idx = refToIndex(ref);
    if (idx < 0 || idx >= draft.elements.length) idx = draft.activeIndex;
    var item = draft.elements[idx];
    if (item.el && item.el.isConnected) return item.el;
    var el = null;
    try { if (item.snapshot.selectorPath) el = document.querySelector(item.snapshot.selectorPath); } catch (e) {}
    if (!el && item.snapshot.xpath) {
      try { el = document.evaluate(item.snapshot.xpath, document, null, 9, null).singleNodeValue; } catch (e) {}
    }
    if (el) item.el = el;
    return item.el;
  }

  // 对指定元素改样式并记进它自己的 styleDiff（heb:aside:apply 按 target 路由用）
  function styleSetOn(item, prop, value) {
    if (!item || !item.el) return;
    try {
      if (!(prop in item.styleDiff)) {
        item.styleDiff[prop] = { before: item.el.style.getPropertyValue(prop), after: value };
      } else {
        item.styleDiff[prop].after = value;
      }
      item.el.style.setProperty(prop, value);
      if ((/border.*width/.test(prop) || prop === "border-color") &&
          window.getComputedStyle(item.el).borderStyle === "none") {
        item.el.style.setProperty("border-style", "solid");
      }
    } catch (e) { /* 静默 */ }
    syncDraftToList();
  }
  // heb:aside:mutate：结构改动（草稿态，刷新即消失）。
  function handleAsideMutate(p) {
    var el = elementForRef(p.target || "@1");
    if (!el) return;
    var desc = "";
    try {
      if (p.op === "append" && p.html) {
        el.insertAdjacentHTML("beforeend", p.html);
        // 追加的新元素不塞进 draft.elements——那是"用户选中元素"集合（@N 编号、
        // 提交账本的主体），append 产物混进去会让编号膨胀、把用户没选的元素也带进
        // 提交。它已记进 structuralChanges（含 html），主对话据此在源码里加元素即可。
        desc = "在 " + (p.target || "@1") + " 内追加了元素";
      } else if (p.op === "remove") {
        el.style.display = "none"; // 草稿态用隐藏代替真删，撤销/找回都还在
        desc = "移除了 " + (p.target || "@1") + "（预览态隐藏）";
      } else if (p.op === "setText") {
        el.textContent = p.text || "";
        desc = (p.target || "@1") + " 文本改为「" + truncate(p.text || "", 40) + "」";
      } else {
        return;
      }
    } catch (e) { return; }
    if (draft) draft.structuralChanges.push({ op: p.op, target: p.target || "@1", html: p.html || null, text: p.text || null, desc: desc });
    syncDraftToList();
    if (cardChat) appendChatMsg(cardChat.msgList, "tool", "🔧 " + desc);
  }

  // <input>/<textarea> 受控写入：React 等框架用原型上的 value setter 装了拦截器追踪
  // 内部状态，直接 el.value= 会被它"吞掉"（值看着变了但框架状态没更新，re-render 时回滚）。
  // 必须调原型原生 setter 绕过拦截器，再派发 input 让框架感知。
  function setNativeInputValue(el, value) {
    var proto = el instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : HTMLInputElement.prototype;
    var setter = Object.getOwnPropertyDescriptor(proto, "value");
    if (setter && setter.set) setter.set.call(el, value);
    else el.value = value;
  }

  // heb:aside:act：页面交互（点击/输入/hover/按键/滚动），触发交互态给用户看
  function handleAsideAct(p) {
    var el = elementForRef(p.target || "@1");
    if (!el && p.action !== "scroll") return;
    try {
      if (p.action === "click") {
        el.click();
      } else if (p.action === "type") {
        el.focus();
        if ("value" in el) {
          setNativeInputValue(el, p.text || "");
          el.dispatchEvent(new Event("input", { bubbles: true }));
          el.dispatchEvent(new Event("change", { bubbles: true }));
        } else {
          el.textContent = p.text || "";
        }
      } else if (p.action === "hover") {
        el.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
        el.dispatchEvent(new MouseEvent("mouseenter", { bubbles: false }));
      } else if (p.action === "press") {
        var k = p.key || "Enter";
        var t = el || document.activeElement || document.body;
        t.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true }));
        t.dispatchEvent(new KeyboardEvent("keyup", { key: k, bubbles: true }));
      } else if (p.action === "scroll") {
        (el || window).scrollBy ? (el || window).scrollBy(0, p.delta || 0) : window.scrollBy(0, p.delta || 0);
      } else {
        return;
      }
    } catch (e) { return; }
    if (cardChat) appendChatMsg(cardChat.msgList, "tool", "🖱 " + p.action + " " + (p.target || "@1"));
  }

  // 元素对话（旁支会话）状态：按元素 key 存会话 + 历史，页面没刷新就一直在
  var asideKeyCounter = 0;
  var asideConvos = {}; // elementKey -> { sessionId, messages: [{role,text}] }
  var cardChat = null;  // 当前卡片打开的聊天：{ elementKey, sessionId, msgList, assistantRow }
  // 修改队列：多个元素的待提交改动按元素累积，统一提交到主对话
  var editQueue = []; // 注释列表 [{ id, badge, draft, conversation, styleDiffs, structuralChanges }]
  var queuePanelEl = null;
  var queuePos = null; // 拖动后记住的位置
  var queueCollapsed = false; // 列表浮窗折叠成一条（仅标题行）

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
    // @ 引用弹层挂在 documentElement 上，重建卡片时一并清掉
    var menu = document.querySelector('[' + OVERLAY_ATTR + '="refmenu"]');
    if (menu && menu.parentNode) menu.parentNode.removeChild(menu);
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
        cardPos = { x: l, y: t }; // 记住，重建卡片（切元素/追加）时保持位置不跳回
      };
      var onUp = function () {
        document.removeEventListener("mousemove", onMove, true);
        document.removeEventListener("mouseup", onUp, true);
      };
      document.addEventListener("mousemove", onMove, true);
      document.addEventListener("mouseup", onUp, true);
    });
  }

  // ─────────────────────── 注释列表浮窗（统一汇总，可拖动） ───────────────────────
  var annotationSeq = 0;

  // draft 的每元素样式 diff → [{ref, badge, diff:[{prop,before,after}]}]（只留有改动的）
  function draftStyleDiffs(d) {
    var out = [];
    d.elements.forEach(function (item, i) {
      var diff = [];
      var props = Object.keys(item.styleDiff);
      for (var j = 0; j < props.length; j++) {
        var p = props[j];
        if (item.styleDiff[p].before !== item.styleDiff[p].after) {
          diff.push({ prop: p, before: item.styleDiff[p].before || "(默认)", after: item.styleDiff[p].after });
        }
      }
      if (diff.length) out.push({ ref: "@" + (i + 1), badge: elementBadge(item.snapshot), diff: diff });
    });
    return out;
  }

  // 改动实时自动进注释列表（upsert；样式 diff / 结构改动 / 对话任一非空就保留，
  // 全空则移除——对应「两区都重置 → 从队列删除」）。draft 持 listId 与列表项关联。
  function syncDraftToList() {
    if (!draft) return;
    var styleDiffs = draftStyleDiffs(draft);
    var conv = (asideConvos[draftChatKey(draft)] && asideConvos[draftChatKey(draft)].messages) || [];
    var hasContent = styleDiffs.length || draft.structuralChanges.length || conv.length;
    var idx = -1;
    if (draft.listId) {
      for (var i = 0; i < editQueue.length; i++) if (editQueue[i].id === draft.listId) { idx = i; break; }
    }
    if (!hasContent) {
      if (idx >= 0) editQueue.splice(idx, 1);
      draft.listId = null;
      renderQueuePanel();
      notifyDirty();
      return;
    }
    var item = {
      id: draft.listId,
      badge: elementBadge(draft.elements[0].snapshot) + (draft.elements.length > 1 ? " 等 " + draft.elements.length + " 个元素" : ""),
      draft: draft,
      conversation: conv.slice(),
      styleDiffs: styleDiffs,
      structuralChanges: draft.structuralChanges.slice(),
      submitted: idx >= 0 ? editQueue[idx].submitted : null, // 保留已提交水位
    };
    if (idx >= 0) {
      editQueue[idx] = item;
    } else {
      annotationSeq++;
      item.id = "ann-" + annotationSeq;
      draft.listId = item.id;
      editQueue.push(item);
    }
    renderQueuePanel();
    notifyDirty();
  }

  function removeQueueItem(id) {
    editQueue = editQueue.filter(function (q) { return q.id !== id; });
    if (draft && draft.listId === id) draft.listId = null; // 解除关联，后续改动新建项
    renderQueuePanel();
    notifyDirty();
  }

  // 未提交注释数上行宿主（防丢失警告用）——已全部提交过的项不算
  function notifyDirty() {
    send("heb:annotation:dirty", { count: editQueue.filter(annotationHasDelta).length });
  }

  // 注释项 → 提交载荷（snapshot 取各元素、对话原文、样式 diff、结构改动）。
  // 已提交过的部分（item.submitted 水位）不再重复提交——只发增量。
  function annotationDelta(item) {
    var sub = item.submitted || { styleKeys: {}, structCount: 0, convCount: 0 };
    var styleDiffs = [];
    item.styleDiffs.forEach(function (s) {
      var diff = s.diff.filter(function (d) {
        return sub.styleKeys[s.ref + "|" + d.prop] !== d.after;
      });
      if (diff.length) styleDiffs.push({ ref: s.ref, badge: s.badge, diff: diff });
    });
    return {
      styleDiffs: styleDiffs,
      structuralChanges: item.structuralChanges.slice(sub.structCount),
      conversation: item.conversation.slice(sub.convCount),
    };
  }

  function annotationHasDelta(item) {
    var d = annotationDelta(item);
    return d.styleDiffs.length > 0 || d.structuralChanges.length > 0 || d.conversation.length > 0;
  }

  // 提交后记水位：样式按 ref|prop→after 值、结构/对话按条数
  function markSubmitted(item) {
    var keys = (item.submitted && item.submitted.styleKeys) || {};
    item.styleDiffs.forEach(function (s) {
      s.diff.forEach(function (d) { keys[s.ref + "|" + d.prop] = d.after; });
    });
    item.submitted = {
      styleKeys: keys,
      structCount: item.structuralChanges.length,
      convCount: item.conversation.length,
    };
  }

  function annotationPayload(item) {
    var delta = annotationDelta(item);
    return {
      elements: item.draft.elements.map(function (e, i) {
        return { ref: "@" + (i + 1), snapshot: e.snapshot };
      }),
      conversation: delta.conversation,
      styleDiffs: delta.styleDiffs,
      structuralChanges: delta.structuralChanges,
      // selector 整组改动（PreviewStyle target=selector）：主对话要按"共享组件/类"落地
      selectorStyleChanges: item.draft.selectorStyleChanges || [],
    };
  }

  // 全量载荷（忽略增量水位）：再次提交用——把该注释已提交过的内容重新发一遍主对话。
  function fullAnnotationPayload(item) {
    return {
      elements: item.draft.elements.map(function (e, i) {
        return { ref: "@" + (i + 1), snapshot: e.snapshot };
      }),
      conversation: item.conversation.slice(),
      styleDiffs: item.styleDiffs,
      structuralChanges: item.structuralChanges.slice(),
      selectorStyleChanges: item.draft.selectorStyleChanges || [],
    };
  }

  // 再次提交一条已提交的注释（bug 修复：分割线上方内容此前无法重发）。
  function resubmitAnnotation(item) {
    send("heb:annotation:submit-all", {
      surface: window.__HEB_POPOUT__ ? "popout" : "embedded",
      items: [fullAnnotationPayload(item)],
    });
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
      // 默认放左下角：注释卡片占右侧（最高 84vh、z-index 更高），放右下会被整张盖住
      var pos = queuePos ? "left:" + queuePos.x + "px;top:" + queuePos.y + "px;" : "left:16px;bottom:16px;";
      queuePanelEl.style.cssText = "position:fixed;" + pos + "width:288px;max-height:62vh;display:flex;flex-direction:column;z-index:2147483646;" +
        "background:#fff;color:#1f2328;border:1px solid #d9dde3;border-radius:10px;box-shadow:0 8px 30px rgba(15,23,42,0.18);" +
        "font-family:-apple-system,system-ui,sans-serif;overflow:hidden;";
      queuePanelEl.addEventListener("click", function (e) { e.stopPropagation(); }, false);
      queuePanelEl.addEventListener("mousedown", function (e) { e.stopPropagation(); }, false);
      document.documentElement.appendChild(queuePanelEl);
    }
    queuePanelEl.innerHTML = "";
    var head = document.createElement("div");
    head.style.cssText = "display:flex;align-items:center;gap:6px;padding:8px 10px;cursor:move;user-select:none;font-size:12px;font-weight:500;" + (queueCollapsed ? "" : "border-bottom:1px solid #d9dde3;");
    var headTitle = document.createElement("span");
    headTitle.textContent = "注释列表 (" + editQueue.length + ")";
    headTitle.style.cssText = "flex:1;";
    // 折叠成一条：只留标题行，正文/底部按钮全部收起
    var collapseBtn = document.createElement("button");
    collapseBtn.textContent = queueCollapsed ? "▸" : "▾";
    collapseBtn.title = queueCollapsed ? "展开" : "折叠成一条";
    collapseBtn.style.cssText = "border:none;background:none;color:#8c949e;font-size:12px;cursor:pointer;padding:0 2px;";
    collapseBtn.addEventListener("click", function (e) {
      e.stopPropagation();
      queueCollapsed = !queueCollapsed;
      renderQueuePanel();
    });
    head.appendChild(headTitle); head.appendChild(collapseBtn);
    makeQueueDraggable(queuePanelEl, head);
    if (queueCollapsed) {
      queuePanelEl.appendChild(head);
      return;
    }
    var list = document.createElement("div");
    markScrollable(list);
    list.style.cssText = "flex:1;min-height:0;overflow-y:auto;padding:6px 8px;";
    editQueue.forEach(function (item) {
      var row = document.createElement("div");
      row.style.cssText = "padding:6px 0;border-bottom:1px solid #f0f2f4;cursor:pointer;";
      row.title = "点击重新展开这条注释继续编辑";
      var top = document.createElement("div");
      top.style.cssText = "display:flex;align-items:center;gap:6px;";
      var b = document.createElement("span");
      b.textContent = item.badge;
      b.style.cssText = "flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font:11px ui-monospace,monospace;color:#1f2328;";
      var del = document.createElement("button");
      del.textContent = "×";
      del.style.cssText = "border:none;background:none;color:#8c949e;font-size:15px;line-height:1;cursor:pointer;padding:0 2px;";
      del.addEventListener("click", function (e) { e.stopPropagation(); removeQueueItem(item.id); });
      top.appendChild(b); top.appendChild(del);
      var summary = document.createElement("div");
      markScrollable(summary);
      // 单项限高内滚：改动条目多时不把整个浮窗撑爆
      summary.style.cssText = "margin-top:2px;font:10px ui-monospace,monospace;color:#6e7681;white-space:pre-wrap;max-height:110px;overflow-y:auto;";
      // 已提交的改动灰显在分割线上方，未提交的在下方——一眼看出哪些还没发
      var delta = annotationDelta(item);
      var doneLines = [];
      var newLines = [];
      var newStyleSet = {};
      delta.styleDiffs.forEach(function (s) {
        s.diff.forEach(function (d) { newStyleSet[s.ref + "|" + d.prop] = true; });
      });
      item.styleDiffs.forEach(function (s) {
        s.diff.forEach(function (d) {
          var line = s.ref + " " + d.prop + ": " + d.before + " → " + d.after;
          (newStyleSet[s.ref + "|" + d.prop] ? newLines : doneLines).push(line);
        });
      });
      var subStruct = (item.submitted && item.submitted.structCount) || 0;
      item.structuralChanges.forEach(function (c, ci) {
        (ci < subStruct ? doneLines : newLines).push("🔧 " + c.desc);
      });
      var subConv = (item.submitted && item.submitted.convCount) || 0;
      if (subConv > 0) doneLines.push("💬 对话 " + subConv + " 条");
      if (item.conversation.length > subConv) newLines.push("💬 新对话 " + (item.conversation.length - subConv) + " 条");
      if (doneLines.length) {
        var doneEl = document.createElement("div");
        doneEl.style.cssText = "color:#a8b0b9;";
        doneEl.textContent = doneLines.join("\n");
        summary.appendChild(doneEl);
        var sep = document.createElement("div");
        sep.style.cssText = "display:flex;align-items:center;gap:6px;color:#a8b0b9;margin:3px 0;";
        var sepL = document.createElement("span");
        sepL.style.cssText = "flex:1;border-top:1px dashed #d9dde3;";
        var sepT = document.createElement("span");
        sepT.style.cssText = "flex:none;font-size:9px;";
        sepT.textContent = "已提交 ↑";
        // 再次提交：把分割线上方这次已提交内容重新发一遍主对话（忽略增量水位）。
        // 用户场景：主对话没按预期改、或想让主对话再处理一次同样的注释。
        var resend = document.createElement("button");
        resend.textContent = "再次提交";
        resend.title = "把上方已提交的改动重新发一次主对话";
        resend.style.cssText = "flex:none;border:1px solid #d0d7de;background:#f6f8fa;color:#0969da;font-size:9px;line-height:1;cursor:pointer;border-radius:4px;padding:2px 6px;";
        resend.addEventListener("click", function (e) {
          e.stopPropagation(); // 不触发 row 的「展开编辑」
          resubmitAnnotation(item);
        });
        var sepR = document.createElement("span");
        sepR.style.cssText = "flex:1;border-top:1px dashed #d9dde3;";
        sep.appendChild(sepL); sep.appendChild(sepT); sep.appendChild(resend); sep.appendChild(sepR);
        summary.appendChild(sep);
      }
      var newEl = document.createElement("div");
      newEl.textContent = newLines.length ? newLines.join("\n") : "（无新改动）";
      if (!newLines.length) newEl.style.color = "#a8b0b9";
      summary.appendChild(newEl);
      row.appendChild(top); row.appendChild(summary);
      // 点击项重新展开：注释项移回 draft，重建卡片继续编辑
      row.addEventListener("click", function () {
        // 重新展开继续编辑：项保留在列表（实时同步，listId 关联 upsert）
        draft = item.draft;
        setActiveElement(Math.min(draft.activeIndex, draft.elements.length - 1));
        showAnnotationCard(null, draft);
      });
      list.appendChild(row);
    });
    var foot = document.createElement("div");
    foot.style.cssText = "display:flex;justify-content:space-between;gap:8px;padding:8px 10px;border-top:1px solid #d9dde3;";
    var clear = mkFlatBtn("清空");
    clear.addEventListener("click", function () { editQueue = []; if (draft) draft.listId = null; renderQueuePanel(); notifyDirty(); });
    var submit = mkPrimaryBtn("全部提交");
    submit.title = "把所有注释交给助手合并总结成一条消息，发进主对话";
    submit.addEventListener("click", function () {
      // 只提交有新改动的项（增量）；全部已提交过则无事可做
      var pending = editQueue.filter(annotationHasDelta);
      if (!pending.length) return;
      send("heb:annotation:submit-all", {
        surface: window.__HEB_POPOUT__ ? "popout" : "embedded",
        items: pending.map(annotationPayload),
      });
      pending.forEach(markSubmitted);
      renderQueuePanel(); // 刷新分割线
      notifyDirty(); // 增量清零，解除防丢失拦截
      // 列表保留：提交后可能还要继续改/再提交；不要就点「清空」
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

  // existingDraft：切换激活元素 / 从注释列表展开时复用既有 draft 重建卡片；
  // 缺省为「新选中一个元素」→ 新建单元素 draft。
  function showAnnotationCard(snap, existingDraft) {
    removeCard();
    if (existingDraft) {
      draft = existingDraft;
      setActiveElement(draft.activeIndex);
      snap = cardSnapshot;
    } else {
      draft = newDraft(selectedTarget, snap);
      setActiveElement(0);
    }
    var card = document.createElement("div");
    card.setAttribute(OVERLAY_ATTR, "card");
    var cardTop = 16;
    // 自动避让：选中元素在屏幕右半 → 卡片靠左；否则靠右。避免卡片盖住正在改的元素。
    // 用户拖动过卡片后记住位置（cardPos），不再自动避让。
    var sideRight = true;
    try {
      if (!cardPos && selectedTarget && selectedTarget.getBoundingClientRect) {
        var tr = selectedTarget.getBoundingClientRect();
        if (tr.left + tr.width / 2 > window.innerWidth / 2) sideRight = false;
      }
    } catch (e) {}
    var horiz = cardPos
      ? "left:" + cardPos.x + "px;"
      : (sideRight ? "right:16px;" : "left:16px;");
    card.style.cssText = [
      "position:fixed", "top:" + (cardPos ? cardPos.y : cardTop) + "px", "width:300px", "height:min(760px, calc(100vh - 32px))",
      "display:flex", "flex-direction:column", "z-index:2147483647",
      "background:#ffffff", "color:#1f2328", "border:1px solid #d9dde3",
      "border-radius:10px", "box-shadow:0 8px 30px rgba(15,23,42,0.16)",
      "font-family:-apple-system,system-ui,sans-serif", "overflow:hidden",
    ].join(";") + ";" + horiz;
    // 卡片内的点击/输入不冒泡到页面（冒泡阶段拦截——按钮自己的 handler 先触发，
    // 再在这里阻止继续冒泡到页面 document；不能用捕获阶段，否则会先于按钮 stopPropagation 把点击吃掉）
    card.addEventListener("click", function (e) { e.stopPropagation(); }, false);
    card.addEventListener("mousedown", function (e) { e.stopPropagation(); }, false);

    var head = document.createElement("div");
    head.style.cssText = "display:flex;align-items:center;gap:8px;padding:8px 10px;border-bottom:1px solid #d9dde3;cursor:move;user-select:none;";
    var badge = document.createElement("span");
    badge.textContent = elementBadge(snap);
    badge.style.cssText = "flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font:12px ui-monospace,monospace;";
    // ➕ 追加选取：再选一个元素进当前注释框（不新建框）
    var addBtn = document.createElement("button");
    addBtn.textContent = "+";
    addBtn.title = "再选一个元素，加进这条注释（对话里说 2、3 指代）";
    addBtn.style.cssText = "border:1px solid #d9dde3;background:#f6f8fa;color:#57606a;font-size:14px;line-height:1;cursor:pointer;border-radius:5px;width:20px;height:20px;flex:none;";
    addBtn.setAttribute("data-heb-addbtn", "1");
    addBtn.addEventListener("click", function () {
      pickerMode = "append";
      addBtn.style.background = "#2f81f7"; // 激活态：正在选取
      addBtn.style.color = "#fff";
      addBtn.style.borderColor = "#2f81f7";
      startPicker();
    });
    var closeBtn = document.createElement("button");
    closeBtn.textContent = "×";
    closeBtn.style.cssText = "border:none;background:none;color:#57606a;font-size:18px;line-height:1;cursor:pointer;padding:0 2px;";
    // 关闭只收起卡片：改动已实时进注释列表，点列表项可重新展开；想丢弃用各区「重置」
    closeBtn.addEventListener("click", function () { draft = null; removeCard(); });
    head.appendChild(badge); head.appendChild(addBtn); head.appendChild(closeBtn);
    // 拖动：按住头部移动卡片（改 left/top，避开 right 定位），避免遮住元素
    makeCardDraggable(card, head);

    // ── 元素小方块行：[1][2][3]…（hover 高亮页面元素，点击切换激活）──
    var chipsRow = null;
    if (draft.elements.length > 1) {
      chipsRow = document.createElement("div");
      chipsRow.style.cssText = "display:flex;align-items:center;gap:6px;padding:6px 10px;border-bottom:1px solid #eaedf0;flex-wrap:wrap;";
      draft.elements.forEach(function (item, i) {
        var chip = document.createElement("span");
        chip.style.cssText = "position:relative;display:inline-flex;";
        var num = document.createElement("button");
        num.textContent = String(i + 1);
        num.title = elementBadge(item.snapshot);
        num.style.cssText = i === draft.activeIndex
          ? "width:22px;height:22px;border:none;background:#2f81f7;color:#fff;border-radius:6px;font-size:11px;cursor:pointer;"
          : "width:22px;height:22px;border:1px solid #d9dde3;background:#f1f3f5;color:#57606a;border-radius:6px;font-size:11px;cursor:pointer;";
        num.addEventListener("mouseenter", function () {
          ensureOverlayLoop();
          hoverTarget = item.el && item.el.isConnected ? item.el : null;
        });
        num.addEventListener("mouseleave", function () { hoverTarget = null; });
        num.addEventListener("click", function () {
          setActiveElement(i);
          showAnnotationCard(item.snapshot, draft); // 重建卡片，样式编辑器切到该元素
        });
        chip.appendChild(num);
        // 对话绑定在 1 号元素上——1 号不可删，只能删后续追加的
        if (i > 0) {
          var del = document.createElement("button");
          del.textContent = "×";
          del.title = "移除这个元素";
          del.style.cssText = "position:absolute;top:-5px;right:-5px;width:13px;height:13px;border:none;background:#8c949e;color:#fff;border-radius:50%;font-size:9px;line-height:13px;padding:0;cursor:pointer;display:none;";
          chip.addEventListener("mouseenter", function () { del.style.display = "block"; });
          chip.addEventListener("mouseleave", function () { del.style.display = "none"; });
          del.addEventListener("click", function (e) {
            e.stopPropagation();
            draft.elements.splice(i, 1);
            if (draft.activeIndex >= draft.elements.length) draft.activeIndex = draft.elements.length - 1;
            setActiveElement(draft.activeIndex);
            showAnnotationCard(cardSnapshot, draft);
          });
          chip.appendChild(del);
        }
        chipsRow.appendChild(chip);
      });
    }

    var elementKey = draftChatKey(draft);

    // ══ 子卡片 1：样式参数（默认折叠成一条；盒模型图常驻不折叠）══
    var styleCard = document.createElement("div");
    styleCard.style.cssText = "border-bottom:1px solid #d9dde3;display:flex;flex-direction:column;flex:none;min-height:0;";
    var styleHead = document.createElement("div");
    styleHead.style.cssText = "display:flex;align-items:center;gap:6px;padding:7px 10px;cursor:pointer;user-select:none;font-size:12px;color:#1f2328;";
    var chevron = document.createElement("span");
    chevron.textContent = "▸";
    chevron.style.cssText = "color:#8c949e;font-size:10px;";
    var styleTitle = document.createElement("span");
    styleTitle.textContent = "样式参数";
    styleTitle.style.cssText = "flex:1;font-weight:500;";
    styleHead.appendChild(chevron); styleHead.appendChild(styleTitle);
    // 盒模型图：常驻显示，不随「样式参数」折叠收起——它是最常看的，折进去反而费一次点击。
    var boxModel = buildBoxModel(); // Chrome F12 式盒模型图
    // 可折叠体：字段（margin/padding 之外的视觉属性）+ 全部 CSS 列表 + 两个重置按钮。
    var styleBody = document.createElement("div");
    styleBody.style.cssText = "display:none;flex-direction:column;flex:none;";
    var fields = document.createElement("div");
    fields.style.cssText = "padding:8px 10px;";
    for (var i = 0; i < CARD_FIELDS.length; i++) fields.appendChild(cardRow(CARD_FIELDS[i]));
    var cssList = buildCssList(); // 全部 CSS（折叠）
    // 改动实时自动进注释列表（styleSet → syncDraftToList），无需手动「加入」。
    // 两区各自「重置」：样式参数区重置在 fields 尾部，盒模型+全部 CSS 的在 cssList 尾部。
    var fieldsReset = document.createElement("div");
    fieldsReset.style.cssText = "display:flex;justify-content:flex-end;padding:0 10px 6px;";
    var frBtn = mkFlatBtn("重置样式参数");
    frBtn.title = "还原上面字段刚才的修改；两边都重置后这条会从注释列表消失";
    frBtn.addEventListener("click", function () { styleRevertSrc("fields"); showAnnotationCard(null, draft); });
    fieldsReset.appendChild(frBtn);
    var cssReset = document.createElement("div");
    cssReset.style.cssText = "display:flex;justify-content:flex-end;padding:0 10px 8px;";
    var crBtn = mkFlatBtn("重置 CSS 修改");
    crBtn.title = "还原盒模型图与全部 CSS 里刚才的修改；两边都重置后这条会从注释列表消失";
    crBtn.addEventListener("click", function () { styleRevertSrc("css"); showAnnotationCard(null, draft); });
    cssReset.appendChild(crBtn);
    styleBody.appendChild(fields); styleBody.appendChild(fieldsReset); styleBody.appendChild(cssList); styleBody.appendChild(cssReset);
    var styleCollapsed = true; // 默认折叠成一条长条，只露标题 + 常驻盒模型
    styleHead.addEventListener("click", function () {
      styleCollapsed = !styleCollapsed;
      styleBody.style.display = styleCollapsed ? "none" : "flex";
      chevron.textContent = styleCollapsed ? "▸" : "▾";
    });
    // 顺序：标题条 → 常驻盒模型 → 可折叠体
    styleCard.appendChild(styleHead); styleCard.appendChild(boxModel); styleCard.appendChild(styleBody);

    // ══ 子卡片 2：和助手一起改（LLM 对话面板 + 模型选择器）══
    var chatCard = document.createElement("div");
    chatCard.style.cssText = "display:flex;flex-direction:column;flex:none;";
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
    markScrollable(msgList);
    // 固定高度 + 子滚动条：在整卡全局滚动里对话区有自己的可滚视口，不挤样式区
    msgList.style.cssText = "display:flex;flex-direction:column;gap:6px;padding:6px 10px;overflow-y:auto;height:220px;max-height:34vh;flex:none;";
    var chatInputRow = document.createElement("div");
    chatInputRow.style.cssText = "display:flex;flex-direction:column;gap:4px;padding:6px 10px;border-top:1px solid #d9dde3;flex:none;";
    // 输入框上方固定一排元素标签：发送时全部元素自动随消息带给助手（XML 前缀），
    // 用户直接用自然语言说「1」「2」即可，无需 @ 引用。
    var refsRow = document.createElement("div");
    refsRow.style.cssText = "display:flex;gap:4px;flex-wrap:wrap;align-items:center;";
    draft.elements.forEach(function (item, i) {
      var tag = document.createElement("span");
      tag.style.cssText = "position:relative;display:inline-flex;max-width:130px;background:#ddf4ff;color:#0969da;border:1px solid #b6e3ff;border-radius:4px;padding:1px 6px;font-size:11px;cursor:default;user-select:none;";
      var txt = document.createElement("span");
      txt.textContent = (i + 1) + " " + elementBadge(item.snapshot);
      txt.style.cssText = "overflow:hidden;text-overflow:ellipsis;white-space:nowrap;";
      tag.title = "对话里直接说「" + (i + 1) + "」指代它；悬停可在页面上高亮";
      tag.appendChild(txt);
      tag.addEventListener("mouseenter", function () {
        ensureOverlayLoop();
        hoverTarget = item.el && item.el.isConnected ? item.el : null;
      });
      tag.addEventListener("mouseleave", function () { hoverTarget = null; });
      // 对话绑定在第一个元素（elementKey 即它）——1 号不可删；后续可移除
      if (i > 0) {
        var rm = document.createElement("button");
        rm.textContent = "×";
        rm.title = "移除这个元素引用";
        rm.style.cssText = "position:absolute;top:-5px;right:-5px;width:13px;height:13px;border:none;background:#8c949e;color:#fff;border-radius:50%;font-size:9px;line-height:13px;padding:0;cursor:pointer;display:none;";
        tag.addEventListener("mouseenter", function () { rm.style.display = "block"; });
        tag.addEventListener("mouseleave", function () { rm.style.display = "none"; });
        rm.addEventListener("click", function (e) {
          e.stopPropagation();
          hoverTarget = null;
          draft.elements.splice(i, 1);
          if (draft.activeIndex >= draft.elements.length) draft.activeIndex = draft.elements.length - 1;
          setActiveElement(draft.activeIndex);
          syncDraftToList();
          showAnnotationCard(null, draft);
        });
        tag.appendChild(rm);
      }
      refsRow.appendChild(tag);
    });
    var inputLine = document.createElement("div");
    inputLine.style.cssText = "display:flex;gap:6px;align-items:flex-end;";
    var chatInput = document.createElement("textarea");
    chatInput.rows = 2;
    chatInput.placeholder = draft.elements.length > 1
      ? "让它改这些元素，说 1、2 指代上方标签（⌘↵ 发送）"
      : "让它改这个元素（⌘↵ 发送）";
    chatInput.style.cssText = "flex:1;min-height:36px;max-height:96px;resize:none;background:#f6f8fa;color:#1f2328;border:1px solid #d9dde3;border-radius:6px;font-size:12px;padding:6px;outline:none;font-family:inherit;";
    // 未发送草稿存进 draft：切元素 / 追加选取会重建整张卡片（chatInput 是新 textarea），
    // 不存就丢。draft 是注释的稳定载体，重建时回填，输入不随切元素消失。
    if (draft.chatDraft) chatInput.value = draft.chatDraft;
    chatInput.addEventListener("input", function () { draft.chatDraft = chatInput.value; });
    // 粘贴截图：贴进输入框 → 缩略图行预览，发送时随消息带给助手。
    // 上行通道是 URL 导航（wry heb-bridge），过大的 base64 会撑爆 URL——
    // canvas 等比缩到 ≤1280px 并转 JPEG 压体积。
    var pendingImages = []; // [{ mediaType, data(base64 无前缀) }]
    var imagesRow = document.createElement("div");
    imagesRow.style.cssText = "display:none;gap:4px;flex-wrap:wrap;align-items:center;";
    function renderImagesRow() {
      imagesRow.innerHTML = "";
      imagesRow.style.display = pendingImages.length ? "flex" : "none";
      pendingImages.forEach(function (img, i) {
        var wrap = document.createElement("span");
        wrap.style.cssText = "position:relative;display:inline-flex;";
        var thumb = document.createElement("img");
        thumb.src = "data:" + img.mediaType + ";base64," + img.data;
        thumb.style.cssText = "width:36px;height:36px;object-fit:cover;border:1px solid #d9dde3;border-radius:5px;";
        var rm = document.createElement("button");
        rm.textContent = "×";
        rm.style.cssText = "position:absolute;top:-5px;right:-5px;width:14px;height:14px;border:none;background:#8c949e;color:#fff;border-radius:50%;font-size:9px;line-height:14px;padding:0;cursor:pointer;";
        rm.addEventListener("click", function () { pendingImages.splice(i, 1); renderImagesRow(); });
        wrap.appendChild(thumb); wrap.appendChild(rm);
        imagesRow.appendChild(wrap);
      });
    }
    chatInput.addEventListener("paste", function (e) {
      var items = (e.clipboardData && e.clipboardData.items) || [];
      for (var i = 0; i < items.length; i++) {
        if (items[i].type.indexOf("image/") !== 0) continue;
        e.preventDefault();
        var file = items[i].getAsFile();
        if (!file) continue;
        (function (f) {
          var url = URL.createObjectURL(f);
          var im = new Image();
          im.onload = function () {
            // 上行是 URL 导航（heb-bridge），URL 过长会被截断——压到 ≤800px JPEG 控体积
            var scale = Math.min(1, 800 / Math.max(im.width, im.height));
            var c = document.createElement("canvas");
            c.width = Math.round(im.width * scale);
            c.height = Math.round(im.height * scale);
            c.getContext("2d").drawImage(im, 0, 0, c.width, c.height);
            URL.revokeObjectURL(url);
            var dataUrl = c.toDataURL("image/jpeg", 0.7);
            pendingImages.push({ mediaType: "image/jpeg", data: dataUrl.slice(dataUrl.indexOf(",") + 1) });
            renderImagesRow();
          };
          im.src = url;
        })(file);
      }
    });

    var chatSend = mkPrimaryBtn("发送");
    // 运行中态：消息区末尾跳动点 + 发送按钮变「停止」；heb:aside:done/error 解除。
    // 停止（C6）：点击发 heb:aside:stop，后端置位 cancel flag 中断 agent loop。
    var spinnerRow = null;
    // 前端看门狗：spinner 解除只依赖后端 done/error 事件，万一事件通道断了（eval 下行
    // 失败 / 后端 spawn 异常没回传），spinner 会永转。这里兜底——开跑时起一个超时，
    // 到点还在转就强制解除并提示。后端也有 180s 看门狗，这里设更长（200s）只兜「事件
    // 根本没来」的极端情况，正常路径由 done/error 提前清掉它。
    var asideWatchdog = null;
    function clearAsideWatchdog() {
      if (asideWatchdog) { clearTimeout(asideWatchdog); asideWatchdog = null; }
    }
    function setAsideBusy(busy) {
      if (busy) {
        clearAsideWatchdog();
        asideWatchdog = setTimeout(function () {
          asideWatchdog = null;
          if (chatSend.__busy__) {
            appendChatMsg(msgList, "assistant", "⚠️ 助手好像卡住了，已停止等待。再发一次试试。");
            setAsideBusy(false);
          }
        }, 200000);
        chatSend.disabled = false; // 不再禁用——run 中它是「停止」按钮
        chatSend.textContent = "停止";
        chatSend.style.opacity = "";
        chatSend.__busy__ = true;
        if (!spinnerRow) {
          spinnerRow = document.createElement("div");
          spinnerRow.style.cssText = "align-self:flex-start;padding:2px 6px;color:#57606a;font-size:14px;";
          spinnerRow.textContent = "·";
          var dots = 1;
          spinnerRow.__hebTimer__ = setInterval(function () {
            dots = dots % 3 + 1;
            spinnerRow.textContent = new Array(dots + 1).join("·");
          }, 400);
        }
        msgList.appendChild(spinnerRow); // 始终挪到末尾
        msgList.scrollTop = msgList.scrollHeight;
      } else {
        clearAsideWatchdog();
        chatSend.disabled = false;
        chatSend.textContent = "发送";
        chatSend.style.opacity = "";
        chatSend.__busy__ = false;
        if (spinnerRow) {
          clearInterval(spinnerRow.__hebTimer__);
          if (spinnerRow.parentNode) spinnerRow.parentNode.removeChild(spinnerRow);
          spinnerRow = null;
        }
      }
    }
    var sendChat = function () {
      // run 中点击 = 停止（C6）：发 heb:aside:stop 中断当前 run
      if (chatSend.__busy__) {
        var sid = asideConvos[elementKey] && asideConvos[elementKey].sessionId;
        if (sid) send("heb:aside:stop", { surface: window.__HEB_POPOUT__ ? "popout" : "embedded", sessionId: sid });
        return;
      }
      var t = chatInput.value.trim();
      if (!t && !pendingImages.length) return;
      if (chatSend.disabled) return;
      chatInput.value = "";
      if (draft) draft.chatDraft = ""; // 已发送，清掉草稿（避免重建卡片又回填旧文本）
      appendChatMsg(msgList, "user", t + (pendingImages.length ? "（含 " + pendingImages.length + " 张截图）" : ""));
      asideConvos[elementKey] = asideConvos[elementKey] || { sessionId: null, messages: [] };
      asideConvos[elementKey].messages.push({ role: "user", text: t });
      cardChat.assistantRow = null;
      setAsideBusy(true);
      var sel = modelSelect.value ? modelSelect.value.split("|") : ["", ""];
      send("heb:aside:send", {
        surface: window.__HEB_POPOUT__ ? "popout" : "embedded",
        elementKey: elementKey,
        sessionId: asideConvos[elementKey].sessionId,
        text: t || "（见截图）",
        images: pendingImages.slice(),
        providerId: sel[0] || undefined,
        model: sel[1] || undefined,
        element: elementLocator(cardSnapshot),
        // 全部选中元素的 @N → 定位映射，宿主拼进每轮 user content 前缀
        elements: draft.elements.map(function (item, i) {
          return { ref: "@" + (i + 1), locator: elementLocator(item.snapshot) };
        }),
      });
      pendingImages = [];
      renderImagesRow();
    };
    chatSend.addEventListener("click", sendChat);
    chatInput.addEventListener("keydown", function (e) { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); sendChat(); } });
    inputLine.appendChild(chatInput); inputLine.appendChild(chatSend);
    chatInputRow.appendChild(refsRow); chatInputRow.appendChild(imagesRow); chatInputRow.appendChild(inputLine);
    // 对话区与输入框之间的可拖动分割线：上下拖改 msgList 高度（扩大/缩小 chat 区）。
    var chatResizer = document.createElement("div");
    chatResizer.style.cssText = "height:7px;flex:none;cursor:ns-resize;background:#f0f2f5;border-top:1px solid #d9dde3;border-bottom:1px solid #d9dde3;display:flex;align-items:center;justify-content:center;";
    var grip = document.createElement("div");
    grip.style.cssText = "width:28px;height:3px;border-radius:2px;background:#c2c8d0;";
    chatResizer.appendChild(grip);
    chatResizer.title = "拖动调整对话区高度";
    chatResizer.addEventListener("mousedown", function (e) {
      e.preventDefault();
      var startY = e.clientY;
      var startH = msgList.getBoundingClientRect().height;
      // 拖动期间解除 34vh 上限，让对话区能拉高；松手后保留显式高度。
      msgList.style.maxHeight = "none";
      var onMove = function (ev) {
        var next = startH + (ev.clientY - startY);
        next = Math.max(120, Math.min(next, window.innerHeight * 0.7));
        msgList.style.height = next + "px";
        msgList.scrollTop = msgList.scrollHeight;
      };
      var onUp = function () {
        document.removeEventListener("mousemove", onMove, true);
        document.removeEventListener("mouseup", onUp, true);
        document.body.style.userSelect = "";
      };
      document.body.style.userSelect = "none";
      document.addEventListener("mousemove", onMove, true);
      document.addEventListener("mouseup", onUp, true);
    });
    var chatFoot = document.createElement("div");
    chatFoot.style.cssText = "display:flex;justify-content:space-between;align-items:center;gap:8px;padding:6px 10px;border-top:1px solid #d9dde3;flex:none;";
    var footHint = document.createElement("span");
    footHint.style.cssText = "flex:1;min-width:0;font-size:10px;color:#8c949e;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;";
    footHint.textContent = "改动自动进左下注释列表";
    // 提交当前这条注释：复用「全部提交」体系（只提交本 draft 对应项），不再走已废弃的
    // 单条直提路径——两套提交并存只会让用户困惑哪个生效。
    var submitMain = mkPrimaryBtn("提交这条");
    submitMain.title = "把这条注释交给助手总结成一条消息，发进主对话";
    submitMain.addEventListener("click", function () {
      syncDraftToList(); // 确保最新改动已落进列表项
      var item = null;
      for (var qi = 0; qi < editQueue.length; qi++) {
        if (editQueue[qi].id === (draft && draft.listId)) { item = editQueue[qi]; break; }
      }
      if (!item) {
        appendChatMsg(msgList, "assistant", "（还没有可提交的改动）");
        return;
      }
      appendChatMsg(msgList, "assistant", "正在总结并提交到主对话…");
      if (annotationHasDelta(item)) {
        // 有新增量：发增量并记水位
        send("heb:annotation:submit-all", {
          surface: window.__HEB_POPOUT__ ? "popout" : "embedded",
          items: [annotationPayload(item)],
        });
        markSubmitted(item);
      } else {
        // 无新增量但之前提过：重新发全量（用户想再让主对话处理一次）
        resubmitAnnotation(item);
      }
      renderQueuePanel();
      notifyDirty();
    });
    footHint && chatFoot.appendChild(footHint);
    chatFoot.appendChild(submitMain);
    chatCard.appendChild(chatHead); chatCard.appendChild(msgList); chatCard.appendChild(chatResizer); chatCard.appendChild(chatInputRow); chatCard.appendChild(chatFoot);

    cardChat = { elementKey: elementKey, sessionId: (asideConvos[elementKey] && asideConvos[elementKey].sessionId) || null, msgList: msgList, assistantRow: null, modelSelect: modelSelect, setBusy: setAsideBusy };
    if (asideConvos[elementKey]) {
      for (var m = 0; m < asideConvos[elementKey].messages.length; m++) {
        var hm = asideConvos[elementKey].messages[m];
        if (hm.role === "style" && hm.style) {
          // 重建对话流里的样式改动块：按 locate 重新查 DOM 找回元素（卡片重建/React
          // 重渲染后旧 DOM 引用已失效），还原/重做按钮才能真正作用到当前页面元素。
          var rEls = resolveStyleEls(hm.style.locate, document);
          appendStyleChange(msgList, {
            label: hm.style.label, prop: hm.style.prop, value: hm.style.value,
            els: rEls, before: hm.style.before ? hm.style.before.slice() : [],
            locate: hm.style.locate,
          });
        } else {
          appendChatMsg(msgList, hm.role, hm.text);
        }
      }
    }
    // 请求模型列表填充选择器
    send("heb:aside:models:request", { surface: window.__HEB_POPOUT__ ? "popout" : "embedded" });

    card.appendChild(head);
    // head 固定，其余进全局滚动区：样式区/对话区自然堆叠不互挤，整卡一根滚动条
    var cardScroll = document.createElement("div");
    markScrollable(cardScroll);
    cardScroll.style.cssText = "flex:1;min-height:0;overflow-y:auto;display:flex;flex-direction:column;overscroll-behavior:contain;";
    if (chipsRow) cardScroll.appendChild(chipsRow);
    cardScroll.appendChild(styleCard);
    cardScroll.appendChild(chatCard);
    card.appendChild(cardScroll);
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
    // 非样式消息打断连续样式组（user/assistant/tool 文字进来 → 后续样式改动另起外框）
    if (role !== "style") closeStyleGroup(msgList);
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

  // 观察工具调用块（C5）：PreviewInspect/PreviewCapture，hover 显示完整入参 JSON。
  // 与样式块不同——它不改页面、无还原，纯展示"调过这个工具"。
  function appendToolCall(msgList, name, input) {
    closeStyleGroup(msgList);
    var icon = name === "PreviewCapture" ? "📷" : "🔍";
    var brief = "";
    if (input) {
      if (input.what) brief = input.what + (input.selector ? " " + input.selector : "");
      else if (input.selector) brief = input.selector;
    }
    var row = document.createElement("div");
    row.style.cssText = "align-self:flex-start;max-width:90%;display:flex;align-items:center;gap:5px;background:#eef3fb;color:#0b62c4;border-radius:8px;padding:4px 8px;font:11px ui-monospace,monospace;cursor:help;";
    var label = document.createElement("span");
    label.style.cssText = "overflow:hidden;text-overflow:ellipsis;white-space:nowrap;";
    label.textContent = icon + " " + name + (brief ? " · " + brief : "");
    // hover 详情：完整入参 JSON（原生 title，简单可靠）
    var detail = "";
    try { detail = JSON.stringify(input || {}, null, 2); } catch (e) { detail = String(input); }
    row.title = name + "\n" + detail;
    row.appendChild(label);
    msgList.appendChild(row);
    msgList.scrollTop = msgList.scrollHeight;
    return row;
  }

  // 样式改动块（C7）：每次 PreviewStyle 渲染一个可还原/重做的块；连续多个改动归入
  // 同一外框，外框带统一还原/重做（P 图软件 before/after 对比）。状态机：
  // applied（已应用 after）⇄ reverted（已还原 before）。还原/重做直接操作元素内联样式。
  // msgList.__styleGroup__ 持当前活跃外框；非样式消息进来时（下方 appendChatMsg/其它
  // 渲染）调 closeStyleGroup 收尾，保证"连续"语义。
  function closeStyleGroup(msgList) {
    msgList.__styleGroup__ = null;
  }
  // 把 change 作用的元素设到 after（改后值）或 before（改前值）。toAfter=false 时
  // 用 before[i]，无记录则 removeProperty（回到无内联）。重建后 els 数量可能与 before
  // 不齐——越界取 undefined 走 removeProperty 降级，安全。纯设样式，不动持久状态标志。
  function setChangeEls(change, toAfter) {
    for (var i = 0; i < change.els.length; i++) {
      var el = change.els[i];
      if (!el) continue;
      try {
        if (toAfter) {
          el.style.setProperty(change.prop, change.value);
        } else if (change.before[i]) {
          el.style.setProperty(change.prop, change.before[i]);
        } else {
          el.style.removeProperty(change.prop);
        }
      } catch (e) { /* 静默 */ }
    }
  }
  // hover 临时预览：只改样式不动 change.reverted（移开后按真实状态恢复）。
  function previewStyleState(change, toAfter) {
    setChangeEls(change, toAfter);
  }
  function applyStyleState(change, toAfter) {
    setChangeEls(change, toAfter);
    change.reverted = !toAfter;
  }
  function appendStyleChange(msgList, change) {
    change.reverted = false; // 初始为已应用
    // 取/建当前样式外框（连续样式改动共用一个，便于统一还原对比）
    var group = msgList.__styleGroup__;
    if (!group) {
      group = document.createElement("div");
      group.style.cssText = "align-self:flex-start;max-width:92%;border:1px solid #cfe8d6;border-radius:10px;padding:4px;display:flex;flex-direction:column;gap:3px;background:#f4fbf6;";
      group.__changes__ = [];
      var body = document.createElement("div");
      body.style.cssText = "display:flex;flex-direction:column;gap:3px;";
      group.__body__ = body;
      group.appendChild(body);
      // 外框统一还原/重做条（≥2 个改动才显示，单个用块内按钮就够）
      var foot = document.createElement("div");
      foot.style.cssText = "display:none;justify-content:flex-end;gap:8px;padding:2px 4px 0;border-top:1px dashed #cfe8d6;";
      var allBtn = document.createElement("button");
      allBtn.style.cssText = "border:none;background:none;color:#0969da;font-size:10px;cursor:pointer;padding:0;";
      var allReverted = false;
      allBtn.textContent = "全部还原";
      // hover 预览整组翻到对侧；移开按各 change 的真实状态逐个恢复（单条可能被单独点过）。
      allBtn.addEventListener("mouseenter", function () {
        group.__changes__.forEach(function (c) { previewStyleState(c, allReverted); });
      });
      allBtn.addEventListener("mouseleave", function () {
        group.__changes__.forEach(function (c) { previewStyleState(c, !c.reverted); });
      });
      allBtn.addEventListener("click", function () {
        allReverted = !allReverted;
        group.__changes__.forEach(function (c) { applyStyleState(c, !allReverted); if (c.__sync__) c.__sync__(); });
        allBtn.textContent = allReverted ? "全部重做" : "全部还原";
      });
      foot.appendChild(allBtn);
      group.__foot__ = foot;
      group.appendChild(foot);
      msgList.appendChild(group);
      msgList.__styleGroup__ = group;
    }
    group.__changes__.push(change);
    if (group.__changes__.length >= 2) group.__foot__.style.display = "flex";

    // 单个改动块
    var block = document.createElement("div");
    block.style.cssText = "display:flex;align-items:center;gap:6px;font:11px ui-monospace,monospace;color:#1a7f4b;background:#eafaf0;border-radius:6px;padding:3px 7px;";
    var txt = document.createElement("span");
    txt.style.cssText = "flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;";
    txt.textContent = "🎨 " + change.label + " " + change.prop + " → " + change.value;
    txt.title = change.prop + ": " + (change.before[0] || "(无)") + " → " + change.value
      + "（作用 " + change.els.length + " 个元素）";
    var btn = document.createElement("button");
    btn.style.cssText = "flex:none;border:1px solid #b7e0c4;background:#fff;color:#0969da;font-size:10px;cursor:pointer;border-radius:4px;padding:1px 6px;";
    btn.textContent = "还原";
    change.__sync__ = function () { btn.textContent = change.reverted ? "重做" : "还原"; };
    // hover 即时预览：悬停时把样式临时翻到对侧（已应用→显示改前 / 已还原→显示改后），
    // 肉眼对比；移开恢复到当前真实状态。只视觉切换，不动 change.reverted 持久标志。
    // 点击才永久翻转。让用户 hover 一眼看出"这条改动改了什么"再决定要不要还原。
    btn.addEventListener("mouseenter", function () {
      previewStyleState(change, change.reverted); // reverted 时预览=改后(toAfter=true)，反之预览改前
    });
    btn.addEventListener("mouseleave", function () {
      previewStyleState(change, !change.reverted); // 恢复到真实状态对应的样式
    });
    btn.addEventListener("click", function () {
      applyStyleState(change, change.reverted); // reverted 时点=重做(toAfter=true)
      change.__sync__();
    });
    block.appendChild(txt); block.appendChild(btn);
    group.__body__.appendChild(block);
    msgList.scrollTop = msgList.scrollHeight;
    return block;
  }

  /* ───────────────────────── picker 状态机 ───────────────────────── */

  var pickerActive = false;
  // "new" = 选中新建注释框；"append" = 选中追加进当前 draft（➕ 按钮触发，用完即还原）
  var pickerMode = "new";

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
    hoverTarget = null;
    flashSelect(el); // 点中瞬间闪一下，给「按下选中」的反馈
    stopPicker(false);
    // 通知宿主 picker 已结束（选中成功也算结束）→ embedded 模式 React 按钮恢复非激活态
    send("heb:picker:cancelled", {});
    if (pickerMode === "append" && draft) {
      // 追加进当前注释框：去重（同一元素已在列表就只切激活——比 DOM 引用 +
      // selectorPath，React 重渲染换节点也能认出是同一逻辑元素）；重建卡片（不新建 draft）
      pickerMode = "new";
      selectedTarget = el;
      var snap = collectSnapshot(el);
      var dup = findDraftElementIndex(draft, el, snap);
      if (dup >= 0) {
        draft.elements[dup].el = el; // 刷新可能已 detach 的节点引用
        draft.activeIndex = dup;
      } else {
        draft.elements.push({ key: elementKeyOf(el), el: el, snapshot: snap, styleDiff: {} });
        draft.activeIndex = draft.elements.length - 1;
      }
      showAnnotationCard(null, draft);
      return;
    }
    selectedTarget = el;
    styleDiff = {};
    cardPos = null; // 全新选中：让卡片按新元素位置重新自动避让
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
    // ➕ 按钮激活色还原（选中成功会重建卡片自然还原，这里兜 Esc 取消的场景）
    var ab = document.querySelector('[data-heb-addbtn]');
    if (ab) { ab.style.background = "#f6f8fa"; ab.style.color = "#57606a"; ab.style.borderColor = "#d9dde3"; }
    if (cancelled) {
      pickerMode = "new";
      send("heb:picker:cancelled", {});
    }
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
          if (cardChat.setBusy) cardChat.setBusy(true); // spinner 保持在消息流末尾
          cardChat.msgList.scrollTop = cardChat.msgList.scrollHeight;
        }
        break;
      case "heb:aside:apply":
        if (msg.payload) {
          var apTarget = msg.payload.target || "@1";
          var apLabel = apTarget;
          var apEls = []; // 本次改动作用的元素集合（还原/重做按钮要操作它们）
          var apBefore = []; // 与 apEls 一一对应的改前内联值（"" = 原本无此内联属性）
          var apLocateSnap = null; // @N 来源：作用元素的 snapshot，用于算可重建的定位
          if (/^@\d+$/.test(apTarget)) {
            // @N 路由到 draft 里的对应元素；无 draft（旧单元素路径）退回激活元素
            var apItem = null;
            if (draft) {
              var apIdx = refToIndex(apTarget);
              if (apIdx < 0 || apIdx >= draft.elements.length) apIdx = draft.activeIndex;
              apItem = draft.elements[apIdx];
              if (apItem && apItem.el) {
                apEls = [apItem.el];
                apBefore = [apItem.el.style.getPropertyValue(msg.payload.prop)];
              }
              if (apItem) apLocateSnap = apItem.snapshot;
              styleSetOn(apItem, msg.payload.prop, msg.payload.value);
            } else {
              var ct = currentTarget();
              if (ct) { apEls = [ct]; apBefore = [ct.style.getPropertyValue(msg.payload.prop)]; }
              apLocateSnap = cardSnapshot;
              styleApply(msg.payload.prop, msg.payload.value);
            }
          } else {
            // CSS selector：批量/单个直接应用，并记进 draft 的 selector 改动账本
            // （selectorStyleChanges 随注释一起提交，主对话知道这是组级改动）
            try {
              apEls = msg.payload.allMatches
                ? Array.prototype.slice.call(document.querySelectorAll(apTarget))
                : (document.querySelector(apTarget) ? [document.querySelector(apTarget)] : []);
            } catch (e) {}
            for (var ai = 0; ai < apEls.length; ai++) {
              try {
                apBefore.push(apEls[ai].style.getPropertyValue(msg.payload.prop));
                apEls[ai].style.setProperty(msg.payload.prop, msg.payload.value);
              } catch (e) { apBefore.push(""); }
            }
            if (draft) {
              draft.selectorStyleChanges = draft.selectorStyleChanges || [];
              draft.selectorStyleChanges.push({ selector: apTarget, allMatches: !!msg.payload.allMatches, count: apEls.length, prop: msg.payload.prop, value: msg.payload.value });
            }
            apLabel = apTarget + "（" + apEls.length + " 个元素）";
            syncDraftToList();
          }
          if (cardChat) {
            // locate：可持久化的元素定位，重建卡片时据此找回 els（修复切元素后样式块丢失）
            var apLocate = styleChangeLocate(apTarget, msg.payload.allMatches, apLocateSnap);
            var styleChange = {
              label: apLabel, prop: msg.payload.prop, value: msg.payload.value,
              els: apEls, before: apBefore, locate: apLocate,
            };
            appendStyleChange(cardChat.msgList, styleChange);
            // 持久化进对话消息序列：切元素 / 重建卡片时回填能重建这个样式块（含还原入口）。
            // 无 locate（拿不到 selectorPath）的不存——重建时无从找回元素，避免渲染出点了没反应的还原按钮。
            if (apLocate) {
              var styleEk = cardChat.elementKey;
              asideConvos[styleEk] = asideConvos[styleEk] || { sessionId: null, messages: [] };
              asideConvos[styleEk].messages.push({
                role: "style",
                style: { label: apLabel, prop: msg.payload.prop, value: msg.payload.value, before: apBefore.slice(), locate: apLocate },
              });
            }
          }
        }
        break;
      case "heb:aside:mutate":
        if (msg.payload) handleAsideMutate(msg.payload);
        break;
      case "heb:aside:act":
        if (msg.payload) handleAsideAct(msg.payload);
        break;
      case "heb:aside:tool":
        // 观察工具（PreviewInspect/PreviewCapture）调用块，hover 看完整入参（C5）
        if (cardChat && msg.payload) appendToolCall(cardChat.msgList, msg.payload.name, msg.payload.input);
        break;
      case "heb:aside:done":
        if (cardChat && cardChat.assistantRow) {
          var conv = asideConvos[cardChat.elementKey];
          if (conv) conv.messages.push({ role: "assistant", text: cardChat.assistantRow.textContent });
          cardChat.assistantRow = null;
        }
        if (cardChat && cardChat.setBusy) cardChat.setBusy(false);
        syncDraftToList(); // 对话内容也属于注释项，轮次结束同步进列表
        break;
      case "heb:aside:submitted":
        if (cardChat) appendChatMsg(cardChat.msgList, "assistant", "✅ 已提交到主对话，主对话会据此改源码");
        // 提交成功不删列表项——markSubmitted 已记水位（UI 灰显「已提交↑」），
        // 用户可继续改再提交；要清掉点列表的「清空」。
        break;
      case "heb:aside:error":
        if (cardChat && msg.payload) appendChatMsg(cardChat.msgList, "assistant", "⚠️ " + (msg.payload.message || "出错了"));
        if (cardChat && cardChat.setBusy) cardChat.setBusy(false);
        break;
      case "heb:unload:allow":
        // 用户已在工具栏确认丢弃——本次离开不再被 beforeunload 拦（一次性）
        unloadAllowOnce = true;
        break;
      default:
        break;
    }
  }

  // 下行入口：wry 模式 Rust eval 调它；iframe 模式 message 事件喂它。
  window.__HEB_RX__ = handleIn;

  // 未提交注释防丢失兜底：页面自身跳转（链接点击 / location 赋值）触发 beforeunload。
  // 工具栏发起的刷新/导航由 React 侧自定义弹窗拦截并先发 heb:unload:allow 放行，
  // 这里见放行标志就不再拦（一次性，避免双弹）。
  var unloadAllowOnce = false;
  window.addEventListener("beforeunload", function (e) {
    if (unloadAllowOnce) { unloadAllowOnce = false; return; }
    if (editQueue.some(annotationHasDelta)) {
      e.preventDefault();
      e.returnValue = "";
    }
  });
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
