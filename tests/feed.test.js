import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  BLOCKED, answerArgv, answerFailed, answeredResult, continuePrompt, decisionListArgv, feedArgv,
  gone, lastLine, mergeLastGood, messageResult, opener, parseDecisions, parseFeed, problem,
  prProblem, sendArgv, sendOutcome,
} from "../mods/conductor-deck/feed.js";
import { buildCards } from "../mods/conductor-deck/model.js";

// conductor's own feed snapshot (`crates/conductor/src/snapshots/…feed_json.snap`).
const FEED = readFileSync(new URL("./fixtures/feed.json", import.meta.url), "utf8");

test("parses conductor's feed", () => {
  const { repo, tasks, errors } = parseFeed(FEED);
  assert.deepEqual(repo, {
    id: "web-3f9a1c",
    root: "/src/web",
    dir: "/Users/me/.config/conductor/repos/web-3f9a1c",
    terminal: "t11",
    space: "sp1",
  });
  assert.deepEqual(tasks.map((t) => t.id), ["audit-deps-01", "add-docs-02", "fix-login-07", "pick-db-03", "rate-limit-08"]);

  const docs = tasks.find((t) => t.id === "add-docs-02");
  assert.equal(docs.phaseSince, Date.parse("2026-10-14T09:20:00Z"));
  assert.deepEqual(docs.report, {
    verb: "done", text: "Draft PR open", prUrl: "https://github.com/erd/web/pull/42", ts: Date.parse("2026-10-14T09:14:00Z"),
  });
  assert.deepEqual(docs.pr, { state: "open", checks: "pass", review: "approved", ts: null });
  assert.equal(docs.terminal, "t3");

  const db = tasks.find((t) => t.id === "pick-db-03");
  assert.deepEqual(db.decisions, [{ key: "database", title: "Postgres or ClickHouse?", openedAt: Date.parse("2026-10-14T12:15:00Z") }]);

  const brief = tasks.find((t) => t.id === "rate-limit-08");
  assert.equal(brief.terminal, null);
  assert.equal(brief.branch, null);
  assert.equal(brief.phaseSince, null);
});

test("a task conductor couldn't read is an error, not a card", () => {
  const { errors } = parseFeed(FEED);
  assert.equal(errors.length, 1);
  assert.equal(errors[0].id, "broken-09");
  assert.equal(
    problem(errors),
    "task broken-09: /Users/me/.config/conductor/repos/web-3f9a1c/tasks/broken-09/task.json: EOF while parsing an object at line 1 column 1",
  );
  assert.equal(problem([]), null);
  assert.equal(problem([{ id: "b", error: "x" }, { id: "a", error: "y" }]), "task a: y");
});

test("the feed's cards", () => {
  const { tasks } = parseFeed(FEED);
  const { cards } = buildCards(tasks, new Map([["t1", "working"]]));
  assert.deepEqual(cards.map((c) => [c.id, c.state, c.word, c.detail]), [
    ["pick-db-03", "needs_you", "needs permission", "Claude needs your permission to use Bash"],
    ["add-docs-02", "ready_to_merge", "ready to merge", null],
    ["fix-login-07", "in_progress", "working", "Edit src/login.rs"],
    ["rate-limit-08", "pending", "briefed", null],
  ]);
  const all = buildCards(tasks, new Map(), { showHidden: true }).cards;
  assert.equal(all.at(-1).id, "audit-deps-01");
  assert.equal(all.at(-1).word, "torn down");
});

test("a repo with no state has no tasks", () => {
  const feed = parseFeed('{"repo":{"id":"web-3f9a1c","root":"/src/web","dir":"/x"},"tasks":[]}');
  assert.deepEqual(feed.tasks, []);
  assert.equal(feed.repo.terminal, null);
});

test("output that isn't a feed throws", () => {
  assert.throws(() => parseFeed("conductor: not in a repo"));
  assert.throws(() => parseFeed('{"tasks":[]}'), /not a conductor deck feed/);
});

test("an unreadable task keeps its last good entry", () => {
  const { tasks } = parseFeed(FEED);
  const prev = [{ id: "broken-09", phase: "working" }, { id: "gone-01", phase: "working" }];
  const merged = mergeLastGood(prev, parseFeed(FEED));
  assert.equal(merged.length, tasks.length + 1);
  assert.deepEqual(merged.at(-1), { id: "broken-09", phase: "working" });
  assert.equal(mergeLastGood(null, parseFeed(FEED)).length, tasks.length);
});

test("argv for the feed and decisions", () => {
  assert.deepEqual(feedArgv(), ["deck", "--json"]);
  assert.deepEqual(decisionListArgv("pick-db-03", "web-3f9a1c"), [
    "decision", "list", "--open", "--json", "--task", "pick-db-03", "--repo", "web-3f9a1c",
  ]);
  assert.deepEqual(answerArgv({ key: "database", value: "pg", note: "ok", task: "pick-db-03", repo: "web-3f9a1c" }), [
    "decision", "answer", "database", "pg", "--note", "ok", "--by", "deck", "--task", "pick-db-03", "--repo", "web-3f9a1c",
  ]);
  assert.deepEqual(answerArgv({ key: "database", value: "duck db", note: "", task: "t", repo: "r" }), [
    "decision", "answer", "database", "duck db", "--by", "deck", "--task", "t", "--repo", "r",
  ]);
});

test("argv and text for messaging a worker", () => {
  assert.deepEqual(sendArgv("t2", "rebase on main"), ["send", "t2", "rebase on main"]);
  assert.equal(
    continuePrompt("database", "Postgres"),
    "Decision database answered: Postgres. Check `conductor decision list` and continue.",
  );
});

test("hc send's exit codes", () => {
  assert.equal(sendOutcome(0), "sent");
  assert.equal(sendOutcome(9), "blocked");
  assert.equal(sendOutcome(3), "gone");
  assert.equal(sendOutcome(11), "gone");
  assert.equal(sendOutcome(1), "error");

  assert.equal(messageResult("fix-07", { code: 0 }), "sent to fix-07");
  assert.equal(messageResult("fix-07", { code: 9 }), BLOCKED);
  assert.equal(messageResult("fix-07", { code: 3 }), gone("fix-07"));
  assert.equal(messageResult("fix-07", { code: 2, stderr: "warning\nhc: boom\n" }), "message failed: hc: boom");
  assert.equal(messageResult("fix-07", { code: 2, stderr: "" }), "message failed: hc exited with 2");
});

test("what answering says", () => {
  assert.equal(answerFailed({ code: 1, stderr: "conductor: decision `x` was already answered\n" }), "answer failed: conductor: decision `x` was already answered");
  assert.equal(answerFailed({ code: 1, stderr: "  \n" }), "answer failed: conductor exited with 1");
  assert.deepEqual(answeredResult("fix-07", "db", { code: 0 }), { told: true, message: "answered db for fix-07" });
  assert.deepEqual(answeredResult("fix-07", "db", { code: 9 }), { told: false, message: "answered db; worker is waiting on a prompt, so it wasn't told" });
  assert.deepEqual(answeredResult("fix-07", "db", null), { told: false, message: `answered db; ${gone("fix-07")}` });
  assert.deepEqual(answeredResult("fix-07", "db", { code: 5, stderr: "nope" }), { told: false, message: "answered db, but couldn't tell the worker: nope" });
  assert.equal(lastLine(undefined), null);
});

test("parses conductor decision list", () => {
  const out = JSON.stringify([
    {
      task: "pick-db-03", schema: "conductor-state.v2", key: "database", title: "Postgres or ClickHouse?",
      about: "Analytics needs a store.\n- Postgres: one less system\n- ClickHouse: faster scans",
      options: [{ value: "pg", label: "Postgres", hint: "the default" }, { value: "ch", label: "ClickHouse" }],
      recommend: "pg", allow_freeform: true,
    },
    { task: "pick-db-03", schema: "conductor-state.v2", key: "old", title: "Old", about: "", options: [], allow_freeform: true, answer: { value: "x", answered_by: "deck", ts: "2026-10-14T12:00:00Z" } },
  ]);
  assert.deepEqual(parseDecisions(out), [{
    task: "pick-db-03", key: "database", title: "Postgres or ClickHouse?",
    about: "Analytics needs a store.\n- Postgres: one less system\n- ClickHouse: faster scans",
    options: [{ value: "pg", label: "Postgres", hint: "the default" }, { value: "ch", label: "ClickHouse", hint: null }],
    recommend: "pg", allowFreeform: true,
  }]);
  assert.throws(() => parseDecisions("{}"));
});

test("opening PRs", () => {
  assert.equal(opener("macos"), "open");
  assert.equal(opener("linux"), "xdg-open");
  assert.equal(prProblem("fix-07", null), "fix-07: no PR yet");
  assert.equal(prProblem("fix-07", "file:///etc/passwd"), "not a web URL: file:///etc/passwd");
  assert.equal(prProblem("fix-07", "https://github.com/erd/web/pull/42"), null);
  assert.equal(prProblem("fix-07", "http://example.com"), null);
});
