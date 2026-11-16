// Builders for feed tasks, as `parseFeed` returns them.

/** `2026-10-14T<hms>Z` in ms. */
export const ts = (hms) => Date.parse(`2026-10-14T${hms}Z`);

/**
 * A task in `phase` with no session events: the feed's `since` is then its `updated_at`,
 * 11:00:00.
 */
export function task(id, phase, fields = {}) {
  return {
    id,
    title: `Task ${id}`,
    kind: "ship",
    phase,
    phaseSince: null,
    activity: "not_started",
    detail: null,
    since: ts("11:00:00"),
    report: null,
    decisions: [],
    branch: null,
    prUrl: null,
    pr: null,
    terminal: null,
    updatedAt: ts("11:00:00"),
    ...fields,
  };
}

export const report = (verb, hms, text = null, prUrl = null) => ({ verb, text, prUrl, ts: ts(hms) });
export const decision = (key, hms, title = null) => ({ key, title, openedAt: hms ? ts(hms) : null });
export const pr = (state, checks = null, review = null, hms = null) => ({ state, checks, review, ts: hms ? ts(hms) : null });
