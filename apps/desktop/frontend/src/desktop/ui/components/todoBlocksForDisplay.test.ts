import { todoBlocksForDisplay } from "./todoBlocksForDisplay";

type Todo = { id: string; content: string; status: string };

function expectOrder(name: string, actual: string[], expected: string[]) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${name}: expected ${e}, got ${a}`);
  }
}

const fallback = [
  {
    key: "history",
    todos: [{ id: "old", content: "old task", status: "completed" }],
    ts: 1,
    streaming: false,
  },
];

const activeTodos: Todo[] = [
  { id: "active", content: "active task", status: "in_progress" },
];

const activeBlocks = todoBlocksForDisplay(activeTodos, fallback, 2);
expectOrder(
  "uses active run todo snapshot before persisted history",
  activeBlocks.map((block) => block.key),
  ["active-run"]
);
expectOrder(
  "keeps active run todos in the visible block",
  activeBlocks[0].todos.map((todo) => todo.id),
  ["active"]
);

expectOrder(
  "falls back to persisted todo blocks when there is no active snapshot",
  todoBlocksForDisplay([], fallback, 2).map((block) => block.key),
  ["history"]
);
