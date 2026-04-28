export interface KeyboardShortcutEvent {
  key: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
  defaultPrevented?: boolean;
}

export interface KeyboardFocusTarget {
  tagName?: string | null;
  isContentEditable?: boolean;
  getAttribute?: (name: string) => string | null;
  hasAttribute?: (name: string) => boolean;
}

function hasPrimaryModifier(event: KeyboardShortcutEvent) {
  return Boolean(event.metaKey || event.ctrlKey);
}

function keyIs(event: KeyboardShortcutEvent, key: string) {
  return event.key.toLowerCase() === key;
}

export function isNewConversationShortcut(event: KeyboardShortcutEvent) {
  return (
    hasPrimaryModifier(event) &&
    keyIs(event, "n") &&
    !event.shiftKey &&
    !event.altKey
  );
}

export function isGlobalSearchShortcut(event: KeyboardShortcutEvent) {
  return (
    hasPrimaryModifier(event) &&
    keyIs(event, "f") &&
    Boolean(event.shiftKey) &&
    !event.altKey
  );
}

export function isLocalFindShortcut(event: KeyboardShortcutEvent) {
  return (
    hasPrimaryModifier(event) &&
    keyIs(event, "f") &&
    !event.shiftKey &&
    !event.altKey
  );
}

export function shouldSuppressBareEnterOnDocument(
  event: KeyboardShortcutEvent,
  activeElement: KeyboardFocusTarget | null | undefined
) {
  return (
    event.key === "Enter" &&
    !event.defaultPrevented &&
    !event.metaKey &&
    !event.ctrlKey &&
    !event.shiftKey &&
    !event.altKey &&
    !isKeyboardInteractiveTarget(activeElement)
  );
}

function isKeyboardInteractiveTarget(
  target: KeyboardFocusTarget | null | undefined
) {
  if (!target) return false;
  if (target.isContentEditable) return true;

  const tagName = target.tagName?.toUpperCase();
  if (!tagName) return false;
  if (["INPUT", "TEXTAREA", "SELECT", "BUTTON"].includes(tagName)) return true;

  if (tagName === "A" && target.hasAttribute?.("href")) return true;

  const role = target.getAttribute?.("role");
  return Boolean(
    role &&
      [
        "button",
        "checkbox",
        "combobox",
        "link",
        "listbox",
        "menuitem",
        "option",
        "radio",
        "searchbox",
        "switch",
        "tab",
        "textbox",
      ].includes(role)
  );
}
