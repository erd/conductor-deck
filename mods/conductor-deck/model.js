// The deck's cards: each feed task plus its worker's live status from the app becomes a card
// with a state, a section, the reasons it needs you, and when it got there. A port of the
// v0.3.0 deck's `deck/card.rs`. Pure: no herdr imports, so `node --test` covers it.

/** Sections, top to bottom. */
export const SECTIONS = ["needs_you", "review", "in_progress", "todo", "done"];
export const SECTION_LABEL = {
  needs_you: "NEEDS YOU",
  review: "REVIEW",
  in_progress: "IN PROGRESS",
  todo: "TODO",
  done: "DONE",
};

const SECTION_OF = {
  needs_you: "needs_you",
  failed: "needs_you",
  review: "review",
  ready_to_merge: "review",
  in_progress: "in_progress",
  pending: "todo",
  done: "done",
};

/** Phases whose cards are hidden unless the deck shows all. */
export const HIDDEN_PHASES = new Set(["torn_down", "abandoned"]);

const OPEN_PR = new Set(["open", "draft"]);

/** Why a card needs you, in the order they're checked. */
export function attention(task, agentStatus) {
  const reasons = [];
  if (task.activity === "needs_permission" || agentStatus === "blocked") reasons.push("permission");
  if (task.decisions.length) reasons.push("decision");
  const verb = task.report?.verb;
  if (verb === "blocked") reasons.push("blocked");
  else if (verb === "failed" || task.phase === "failed") reasons.push("failed");
  if (task.pr && OPEN_PR.has(task.pr.state)) {
    if (task.pr.checks === "fail") reasons.push("checks_failing");
    if (task.pr.review === "changes_requested") reasons.push("changes_requested");
  }
  return reasons;
}

/** Whether the worker is running: its hooks say so, or the app does. */
export const isWorking = (task, agentStatus) => task.activity === "working" || agentStatus === "working";

const NEEDS_YOU = new Set(["permission", "decision", "blocked", "changes_requested"]);

export function cardState(phase, reasons, working) {
  if (reasons.some((r) => NEEDS_YOU.has(r))) return "needs_you";
  // Only `failed` and `checks_failing` are left.
  if (reasons.length) return "failed";
  switch (phase) {
    case "merged":
    case "torn_down":
    case "abandoned":
      return "done";
    case "awaiting_merge":
      return "ready_to_merge";
    case "failed":
      return "failed";
  }
  // A running worker beats review and the phases before spawning.
  if (working) return "in_progress";
  switch (phase) {
    case "review":
      return "review";
    case "working":
    case "needs_decision":
      return "in_progress";
    default:
      return "pending";
  }
}

const minOf = (xs) => {
  const known = xs.filter((x) => x !== null && x !== undefined);
  return known.length ? Math.min(...known) : null;
};

/** When `reason` began. The feed has no time of its own for a PR yet, so those use `updated_at`. */
function reasonSince(task, reason) {
  switch (reason) {
    // An app-only prompt has no time of its own either.
    case "permission":
      return task.since;
    case "decision":
      return minOf(task.decisions.map((d) => d.openedAt));
    case "blocked":
      return task.report?.ts ?? null;
    case "failed":
      return task.report?.verb === "failed" ? task.report.ts : task.phaseSince;
    case "checks_failing":
    case "changes_requested":
      return task.pr?.ts ?? task.updatedAt;
  }
  return null;
}

/** When the card entered the state that puts it in its section. */
export function cardSince(task, section, reasons) {
  let since;
  if (section === "needs_you") since = minOf(reasons.map((r) => reasonSince(task, r)));
  else if (section === "review" || section === "done") since = task.phaseSince;
  else since = task.since;
  return since ?? task.updatedAt;
}

export function stateWord(card) {
  const has = (r) => card.attention.includes(r);
  switch (card.state) {
    case "needs_you":
      if (has("permission")) return "needs permission";
      if (has("decision")) return "needs decision";
      if (has("blocked")) return "blocked";
      return "changes requested";
    case "failed":
      return has("checks_failing") && !has("failed") ? "checks failing" : "failed";
    case "in_progress":
      if (card.working) return "working";
      return card.activity === "exited" ? "exited" : "idle";
    case "review":
      return "in review";
    case "ready_to_merge":
      return "ready to merge";
    case "pending":
      return card.phase === "intake" ? "intake" : card.phase === "briefed" ? "briefed" : "spawning";
    case "done":
      return card.phase === "torn_down" ? "torn down" : card.phase === "abandoned" ? "abandoned" : "merged";
  }
  return card.state;
}

/** What follows the state word: the prompt, the decision, the report, the tool or the error. */
export function cardDetail(task, reasons) {
  if (reasons.includes("permission")) {
    return task.activity === "needs_permission" && task.detail ? task.detail : null;
  }
  if (task.decisions.length) {
    const keys = task.decisions.map((d) => d.key).sort(cmp);
    return keys.length === 1 ? keys[0] : `${keys[0]} +${keys.length - 1}`;
  }
  for (const verb of ["blocked", "failed"]) {
    if (reasons.includes(verb) && task.report?.verb === verb && task.report.text) return task.report.text;
  }
  if (task.activity === "working" || task.activity === "idle") return task.detail ?? null;
  return null;
}

const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

/** One card for `task`, given its worker's status in the app (`working`, `blocked`, … or null). */
export function card(task, agentStatus = null) {
  const reasons = attention(task, agentStatus);
  const working = isWorking(task, agentStatus);
  const state = cardState(task.phase, reasons, working);
  const section = SECTION_OF[state];
  const keep = state === "needs_you" || state === "failed";
  const c = {
    id: task.id,
    title: task.title,
    kind: task.kind,
    phase: task.phase,
    activity: task.activity,
    state,
    section,
    attention: keep ? reasons : [],
    working,
    since: cardSince(task, section, reasons),
    branch: task.branch,
    prUrl: task.prUrl,
    pr: task.pr,
    terminal: task.terminal,
    agentStatus,
    decisions: [...task.decisions].sort((a, b) => cmpOpened(a, b) || cmp(a.key, b.key)),
    hidden: HIDDEN_PHASES.has(task.phase),
  };
  c.word = stateWord(c);
  // A Done card is one row, with no room for it.
  c.detail = state === "done" ? null : cardDetail(task, c.attention);
  return c;
}

// A decision with no open time sorts first, as Rust orders `None` before `Some`.
const cmpOpened = (a, b) => (a.openedAt ?? -Infinity) - (b.openedAt ?? -Infinity);

const RANK = Object.fromEntries(SECTIONS.map((s, i) => [s, i]));

/** Section, then oldest first, then id. */
export const compareCards = (a, b) =>
  RANK[a.section] - RANK[b.section] || (a.since ?? 0) - (b.since ?? 0) || cmp(a.id, b.id);

/**
 * Every card, sorted. `status` maps a terminal address to its app status. Hidden (torn-down and
 * abandoned) cards are left out unless `showHidden`; `foldDone` leaves Done cards out of `cards`
 * but they're still counted.
 */
export function buildCards(tasks, status = new Map(), { showHidden = false, foldDone = false } = {}) {
  const get = (t) => (t ? (status instanceof Map ? status.get(t) : status[t]) ?? null : null);
  const all = tasks
    .map((t) => card(t, get(t.terminal)))
    .filter((c) => showHidden || !c.hidden)
    .sort(compareCards);
  const counts = Object.fromEntries(SECTIONS.map((s) => [s, 0]));
  for (const c of all) counts[c.section]++;
  const cards = foldDone ? all.filter((c) => c.section !== "done") : all;
  return { cards, counts };
}

// ---- display -------------------------------------------------------------------------------

/** The PR number in a GitHub PR URL, else null. */
export function prNumber(url) {
  if (!url) return null;
  const trimmed = url.replace(/\/$/, "");
  const at = trimmed.lastIndexOf("/pull/");
  if (at < 0) return null;
  const n = trimmed.slice(at + "/pull/".length);
  return /^\d+$/.test(n) ? n : null;
}

const PR_STATE = { draft: "draft", open: "ready", merged: "merged", closed: "closed" };
const REVIEW = { approved: "approved", changes_requested: "changes requested", review_required: "review required" };

/** The PR line's parts, or null when the card has no PR. */
export function prLine(c) {
  if (!c.pr && !c.prUrl) return null;
  const n = prNumber(c.prUrl);
  return {
    label: n ? `PR #${n}` : "PR",
    state: c.pr ? PR_STATE[c.pr.state] ?? c.pr.state : null,
    checks: c.pr?.checks ?? null,
    review: c.pr?.review ? REVIEW[c.pr.review] ?? c.pr.review : null,
    reviewTone: c.pr?.review === "approved" ? "good" : c.pr?.review === "changes_requested" ? "bad" : "plain",
  };
}

const MINUTE = 60e3;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const pad = (n) => String(n).padStart(2, "0");

/** An elapsed time to the minute, in two units at most: `<1m`, `4m`, `1h 05m`, `3d 04h`. */
export function elapsed(ms) {
  const t = Math.max(0, Math.floor(ms));
  if (t < MINUTE) return "<1m";
  if (t < HOUR) return `${Math.floor(t / MINUTE)}m`;
  if (t < DAY) return `${Math.floor(t / HOUR)}h ${pad(Math.floor((t % HOUR) / MINUTE))}m`;
  return `${Math.floor(t / DAY)}d ${pad(Math.floor((t % DAY) / HOUR))}h`;
}

/** Ms until any of `cards`' elapsed times changes, or null with nothing to show. */
export function nextChangeIn(cards, now) {
  let next = null;
  for (const c of cards) {
    if (c.since === null || c.since === undefined) continue;
    const e = now - c.since;
    // A card from the future reads `<1m` until it isn't.
    const wait = e < 0 ? -e + MINUTE : e < DAY ? MINUTE - (e % MINUTE) : HOUR - (e % HOUR);
    if (next === null || wait < next) next = wait;
  }
  return next;
}

// ---- selection -----------------------------------------------------------------------------

/**
 * The selection after the cards change: the same task if it's still shown, else the same place,
 * clamped. -1 when there's nothing to select.
 */
export function follow(selectedId, prevIndex, cards) {
  if (!cards.length) return -1;
  const at = selectedId ? cards.findIndex((c) => c.id === selectedId) : -1;
  if (at >= 0) return at;
  return Math.min(Math.max(prevIndex, 0), cards.length - 1);
}

/** `index` moved by `delta`, stopping at both ends. */
export function moveBy(index, delta, count) {
  if (!count) return -1;
  return Math.min(Math.max((index < 0 ? 0 : index) + delta, 0), count - 1);
}
