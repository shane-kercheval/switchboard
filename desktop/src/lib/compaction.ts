/// A compaction marker's recap, or null when it has none worth showing. Codex
/// encrypts its summary and sends none, and a blank one has nothing to read
/// either. The marker row and its "Copy summary" action both decide through
/// this, so they can't disagree about whether there is a recap.
export function compactionRecap(summary: string | null): string | null {
  return summary !== null && summary.trim() !== "" ? summary : null;
}
