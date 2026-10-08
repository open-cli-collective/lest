/** Tags offered as filters before the list is expanded. */
export const TAG_FILTER_LIMIT = 12;
/** Tags a flow card shows before the rest fold into a count. */
export const CARD_TAG_LIMIT = 2;

/**
 * The tag filters to show: the most used first, then by name. Collapsed, only
 * the first `limit` and any selected tag; `hidden` counts the rest.
 */
export function tagFilters(
  flowTags: string[][],
  selected: string[],
  expanded: boolean,
  limit = TAG_FILTER_LIMIT,
): { shown: string[]; hidden: number } {
  const counts = new Map<string, number>();
  for (const tags of flowTags) for (const t of new Set(tags)) counts.set(t, (counts.get(t) ?? 0) + 1);
  const all = [...counts.keys()].sort((a, b) => counts.get(b)! - counts.get(a)! || a.localeCompare(b));
  if (expanded || all.length <= limit) return { shown: all, hidden: 0 };
  const shown = all.filter((t, i) => i < limit || selected.includes(t));
  return { shown, hidden: all.length - shown.length };
}
