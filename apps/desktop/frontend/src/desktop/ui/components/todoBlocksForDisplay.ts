export type TodoDisplayBlock<TTodo> = {
  key: string;
  todos: TTodo[];
  ts: number;
  streaming: boolean;
};

export function todoBlocksForDisplay<TTodo>(
  activeTodos: TTodo[] | null | undefined,
  fallbackBlocks: Array<TodoDisplayBlock<TTodo>>,
  now: number = Date.now()
): Array<TodoDisplayBlock<TTodo>> {
  if (activeTodos && activeTodos.length > 0) {
    return [
      {
        key: "active-run",
        todos: activeTodos,
        ts: now,
        streaming: true,
      },
    ];
  }
  return fallbackBlocks;
}
