import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildCards, card, elapsed, follow, moveBy, nextChangeIn, prLine, prNumber,
} from "../mods/conductor-deck/model.js";
import { decision, pr, report, task, ts } from "./helpers.js";

const PHASES = [
  "intake", "briefed", "spawning", "working", "needs_decision", "review",
  "awaiting_merge", "merged", "torn_down", "failed", "abandoned",
];

// The v0.3.0 deck's fixture cases, each as the feed entry conductor folds its event log into.
test("each folded fixture lands in its state", () => {
  const cases = [
    ["needs_permission", "working", { activity: "needs_permission", detail: "Claude needs your permission to use Bash", since: ts("12:00:11") }, "needs_you", false],
    ["decisions", "needs_decision", { activity: "idle", since: ts("12:02:03"), decisions: [decision("auth-flow", "12:02:01")], report: report("blocked", "12:02:02", "Waiting on two decisions") }, "needs_you", false],
    ["pr_lifecycle", "awaiting_merge", { activity: "working", since: ts("12:00:01"), phaseSince: ts("12:08:00"), prUrl: "https://github.com/erd/web/pull/42", pr: pr("merged", "pass", "approved") }, "ready_to_merge", true],
    ["working_tool", "working", { activity: "working", detail: "Edit src/login.rs", since: ts("12:00:01") }, "in_progress", true],
    ["working_between_tools", "working", { activity: "working", since: ts("12:00:01") }, "in_progress", true],
    ["permission_granted", "working", { activity: "working", since: ts("12:00:01") }, "in_progress", true],
    ["idle", "review", { activity: "idle", since: ts("12:05:02"), phaseSince: ts("12:05:30") }, "review", false],
    ["idle_error", "working", { activity: "idle", detail: "rate_limit", since: ts("12:00:20") }, "in_progress", false],
    ["exited", "working", { activity: "exited", detail: "prompt_input_exit", since: ts("12:10:00") }, "in_progress", false],
    ["pr_closed", "working", { activity: "working", pr: pr("closed", "fail") }, "in_progress", true],
    ["idle", "failed", { activity: "idle", report: report("done", "12:05:00", "Draft PR open") }, "failed", false],
    ["exited", "merged", { activity: "exited", detail: "other" }, "done", false],
  ];
  for (const [name, phase, fields, state, working] of cases) {
    const c = card(task("t", phase, fields));
    assert.equal(c.state, state, `${name}/${phase}`);
    assert.equal(c.working, working, `${name}/${phase} working`);
  }
});

test("phases without events", () => {
  const want = {
    intake: "pending", briefed: "pending", spawning: "pending", working: "in_progress",
    needs_decision: "in_progress", review: "review", awaiting_merge: "ready_to_merge",
    failed: "failed", merged: "done", torn_down: "done", abandoned: "done",
  };
  for (const phase of PHASES) assert.equal(card(task("t", phase)).state, want[phase], phase);
});

test("a running worker in review is in progress", () => {
  const c = card(task("t", "review", { activity: "working", detail: "Edit src/login.rs" }));
  assert.equal(c.state, "in_progress");
  assert.equal(c.word, "working");
  assert.equal(c.detail, "Edit src/login.rs");
});

test("needs permission since the prompt", () => {
  const c = card(task("t", "working", { activity: "needs_permission", detail: "Claude needs your permission to use Bash", since: ts("12:00:11") }));
  assert.deepEqual(c.attention, ["permission"]);
  assert.equal(c.since, ts("12:00:11"));
  assert.equal(c.word, "needs permission");
  assert.equal(c.detail, "Claude needs your permission to use Bash");
});

test("an open decision and a blocked report, since the earliest", () => {
  const c = card(task("t", "needs_decision", {
    activity: "idle",
    decisions: [decision("auth-flow", "12:02:01")],
    report: report("blocked", "12:02:02", "Waiting on two decisions"),
  }));
  assert.deepEqual(c.attention, ["decision", "blocked"]);
  assert.equal(c.since, ts("12:02:01"));
  assert.equal(c.word, "needs decision");
  assert.equal(c.detail, "auth-flow");
});

test("a blocked report alone says why", () => {
  const c = card(task("t", "working", { activity: "idle", report: report("blocked", "12:02:02", "Need staging creds") }));
  assert.equal(c.word, "blocked");
  assert.equal(c.detail, "Need staging creds");
  assert.equal(c.since, ts("12:02:02"));
});

test("open decisions oldest first; the detail names the first key", () => {
  const c = card(task("t", "needs_decision", {
    decisions: [decision("alpha", "12:01:00"), decision("zeta", "12:00:00")],
  }));
  assert.deepEqual(c.decisions.map((d) => d.key), ["zeta", "alpha"]);
  assert.equal(c.detail, "alpha +1");
  // One with no open time sorts first.
  const d = card(task("t", "needs_decision", { decisions: [decision("b", "12:00:00"), decision("a", null)] }));
  assert.deepEqual(d.decisions.map((x) => x.key), ["a", "b"]);
});

test("a PR moves between attention and awaiting merge", () => {
  const base = { activity: "idle", phaseSince: ts("12:08:00"), prUrl: "https://github.com/erd/web/pull/42", updatedAt: ts("12:08:00") };
  const failing = card(task("t", "awaiting_merge", { ...base, pr: pr("open", "fail", "review_required", "12:08:00") }));
  assert.deepEqual(failing.attention, ["checks_failing"]);
  assert.equal(failing.state, "failed");
  assert.equal(failing.word, "checks failing");
  assert.equal(failing.since, ts("12:08:00"));

  const changes = card(task("t", "awaiting_merge", { ...base, pr: pr("open", "pass", "changes_requested", "12:20:00") }));
  assert.deepEqual(changes.attention, ["changes_requested"]);
  assert.equal(changes.state, "needs_you");
  assert.equal(changes.word, "changes requested");
  assert.equal(changes.since, ts("12:20:00"));

  const approved = card(task("t", "awaiting_merge", { ...base, pr: pr("open", "pass", "approved", "12:40:00") }));
  assert.equal(approved.state, "ready_to_merge");
  assert.equal(approved.since, ts("12:08:00"));
});

test("without a PR time, PR attention dates from updated_at", () => {
  const c = card(task("t", "review", { pr: pr("draft", "fail"), updatedAt: ts("12:30:00") }));
  assert.deepEqual(c.attention, ["checks_failing"]);
  assert.equal(c.since, ts("12:30:00"));
});

test("a closed PR's failing checks need nothing", () => {
  const c = card(task("t", "working", { pr: pr("closed", "fail", "changes_requested") }));
  assert.deepEqual(c.attention, []);
});

test("a failed phase needs attention, since the phase moved", () => {
  const c = card(task("t", "failed", { activity: "idle", report: report("done", "12:05:00") }));
  assert.deepEqual(c.attention, ["failed"]);
  assert.equal(c.word, "failed");
  // No phase event: updated_at.
  assert.equal(c.since, ts("11:00:00"));
  const moved = card(task("t", "failed", { phaseSince: ts("12:30:00") }));
  assert.equal(moved.since, ts("12:30:00"));
  const reported = card(task("t", "working", { report: report("failed", "12:10:00", "tests won't build") }));
  assert.equal(reported.state, "failed");
  assert.equal(reported.since, ts("12:10:00"));
  assert.equal(reported.detail, "tests won't build");
});

test("the app's status counts", () => {
  const idle = task("t", "working", { activity: "idle", terminal: "t1" });
  const blocked = card(idle, "blocked");
  assert.deepEqual(blocked.attention, ["permission"]);
  assert.equal(blocked.word, "needs permission");
  // The prompt's text is only in the feed.
  assert.equal(blocked.detail, null);
  assert.equal(card(idle, "working").working, true);
  assert.equal(card(idle, "idle").working, false);
  assert.equal(card(idle, "idle").word, "idle");
});

test("state words", () => {
  const word = (phase, fields = {}) => card(task("t", phase, fields)).word;
  assert.equal(word("intake"), "intake");
  assert.equal(word("briefed"), "briefed");
  assert.equal(word("spawning"), "spawning");
  assert.equal(word("working", { activity: "exited" }), "exited");
  assert.equal(word("review"), "in review");
  assert.equal(word("awaiting_merge"), "ready to merge");
  assert.equal(word("merged"), "merged");
  assert.equal(word("torn_down"), "torn down");
  assert.equal(word("abandoned"), "abandoned");
});

test("with no events, a card dates from updated_at", () => {
  const c = card(task("t", "briefed"));
  assert.equal(c.state, "pending");
  assert.equal(c.since, ts("11:00:00"));
  assert.equal(elapsed(ts("11:01:30") - c.since), "1m");
});

test("sorts by section, then oldest first", () => {
  const tasks = [
    task("merged", "merged"),
    task("brief", "briefed"),
    task("working-new", "working", { activity: "working", since: ts("12:05:00") }),
    task("perm-new", "working", { activity: "needs_permission", since: ts("12:09:00") }),
    task("idle", "working", { activity: "idle", since: ts("12:00:00") }),
    task("merge", "awaiting_merge"),
    task("working-old", "working", { activity: "working", since: ts("12:01:00") }),
    task("perm-old", "working", { activity: "needs_permission", since: ts("12:03:00") }),
  ];
  const { cards, counts } = buildCards(tasks);
  assert.deepEqual(cards.map((c) => c.id), [
    "perm-old", "perm-new", "merge", "idle", "working-old", "working-new", "brief", "merged",
  ]);
  assert.deepEqual(counts, { needs_you: 2, review: 1, in_progress: 3, todo: 1, done: 1 });
});

test("ties sort by id", () => {
  const { cards } = buildCards([task("b", "briefed"), task("a", "briefed")]);
  assert.deepEqual(cards.map((c) => c.id), ["a", "b"]);
});

test("hides torn-down and abandoned cards unless asked; folding keeps the count", () => {
  const tasks = [task("live", "working"), task("merged", "merged"), task("down", "torn_down"), task("dropped", "abandoned")];
  assert.deepEqual(buildCards(tasks).cards.map((c) => c.id).sort(), ["live", "merged"]);
  assert.equal(buildCards(tasks, new Map(), { showHidden: true }).cards.length, 4);
  const folded = buildCards(tasks, new Map(), { foldDone: true });
  assert.deepEqual(folded.cards.map((c) => c.id), ["live"]);
  assert.equal(folded.counts.done, 1);
});

test("looks up each worker's status by its terminal", () => {
  const tasks = [task("a", "working", { activity: "idle", terminal: "t1" }), task("b", "working", { activity: "idle", terminal: "t2" })];
  const { cards } = buildCards(tasks, new Map([["t2", "blocked"]]));
  assert.deepEqual(cards.map((c) => [c.id, c.state]), [["b", "needs_you"], ["a", "in_progress"]]);
  assert.equal(buildCards(tasks, { t1: "working" }).cards.find((c) => c.id === "a").working, true);
});

test("formats elapsed time to the minute", () => {
  const s = 1000;
  assert.equal(elapsed(0), "<1m");
  assert.equal(elapsed(59 * s), "<1m");
  assert.equal(elapsed(252 * s), "4m");
  assert.equal(elapsed(3600 * s), "1h 00m");
  assert.equal(elapsed(7500 * s), "2h 05m");
  assert.equal(elapsed(273600 * s), "3d 04h");
  assert.equal(elapsed(-5 * s), "<1m");
});

test("the next redraw is when a shown time next changes", () => {
  const now = ts("12:00:00");
  const at = (since) => ({ since });
  assert.equal(nextChangeIn([], now), null);
  assert.equal(nextChangeIn([at(now - 15e3)], now), 45e3);
  assert.equal(nextChangeIn([at(now - 2 * 3600e3 - 10e3)], now), 50e3);
  // Past a day only the hour shows.
  assert.equal(nextChangeIn([at(now - 86400e3 - 600e3)], now), 3000e3);
  assert.equal(nextChangeIn([at(now - 86400e3 - 600e3), at(now - 30e3)], now), 30e3);
  assert.equal(nextChangeIn([at(now + 5e3)], now), 65e3);
});

test("PR numbers and the PR line", () => {
  assert.equal(prNumber("https://github.com/erd/web/pull/42"), "42");
  assert.equal(prNumber("https://github.com/erd/web/pull/42/"), "42");
  assert.equal(prNumber("https://github.com/erd/web/pull/42/files"), null);
  assert.equal(prNumber("https://github.com/erd/web"), null);
  assert.equal(prNumber(null), null);

  const c = card(task("t", "awaiting_merge", { prUrl: "https://github.com/erd/web/pull/42", pr: pr("open", "pass", "approved") }));
  assert.deepEqual(prLine(c), { label: "PR #42", state: "ready", checks: "pass", review: "approved", reviewTone: "good" });
  assert.deepEqual(prLine(card(task("t", "review", { prUrl: "https://example.com/x" }))), {
    label: "PR", state: null, checks: null, review: null, reviewTone: "plain",
  });
  assert.equal(prLine(card(task("t", "working"))), null);
});

test("the selection starts at the top and stops at the ends", () => {
  assert.equal(follow(null, 0, [{ id: "a" }, { id: "b" }]), 0);
  assert.equal(moveBy(0, -1, 3), 0);
  assert.equal(moveBy(0, 1, 3), 1);
  assert.equal(moveBy(2, 1, 3), 2);
  assert.equal(moveBy(-1, 1, 0), -1);
  assert.equal(follow("a", 0, []), -1);
});

test("the selection follows its task when the order changes", () => {
  const ids = (...xs) => xs.map((id) => ({ id }));
  assert.equal(follow("c", 2, ids("c", "a", "b")), 0);
});

test("the selection keeps its place when its task goes", () => {
  const ids = (...xs) => xs.map((id) => ({ id }));
  assert.equal(follow("c", 2, ids("a", "b", "d", "e")), 2);
  assert.equal(follow("e", 4, ids("a", "b", "d")), 2);
  assert.equal(follow("x", 3, ids("a")), 0);
});
