export const UNNAMED_TEAMMATE = "a teammate";

/** A roster id's display name, never the id itself. */
export function teammateName(id: string, agentNames?: Readonly<Record<string, string>>): string {
  return agentNames?.[id] ?? UNNAMED_TEAMMATE;
}
