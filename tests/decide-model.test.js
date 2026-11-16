import { test } from "node:test";
import assert from "node:assert/strict";
import { aboutBlocks, decision, hints, init, key, onOther, rows, setNote } from "../mods/conductor-deck/decide-model.js";

const option = (value, label, hint = null) => ({ value, label, hint });
const engine = {
  task: "pick-db-03", key: "engine", title: "Which engine?", about: "",
  options: [option("pg", "Postgres"), option("sqlite", "SQLite"), option("mysql", "MySQL")],
  recommend: "sqlite", allowFreeform: true,
};
const auth = {
  task: "pick-db-03", key: "auth", title: "Which auth?", about: "",
  options: [option("oauth", "OAuth"), option("magic", "Magic link")],
  recommend: null, allowFreeform: false,
};
const name = {
  task: "pick-db-03", key: "name", title: "What's it called?", about: "",
  options: [], recommend: null, allowFreeform: true,
};

/** Presses each key in turn, returning the last result. */
function press(s, ...keys) {
  let out = { state: s };
  for (const k of keys) {
    out = typeof k === "string" ? key(out.state, { key: k }) : key(out.state, k);
    if (out.submit || out.cancel) return out;
  }
  return out;
}

test("needs a decision", () => {
  assert.equal(init([]), null);
  assert.equal(init(null), null);
});

test("starts on the recommendation and moves within the options", () => {
  const s = init([engine]);
  assert.equal(rows(s), 4);
  assert.equal(press(s, "enter").submit.value, "sqlite");
  assert.equal(press(s, "k", "up", "enter").submit.value, "pg");
  assert.equal(press(s, "j", "j", "j", "j", "up", "enter").submit.value, "mysql");
});

test("tab moves to a note that goes with the answer", () => {
  let s = init([engine]);
  s = press(s, "tab").state;
  assert.equal(s.focus, "note");
  // In the note, letters are the Input's.
  assert.equal(key(s, { key: "j" }).handled, false);
  s = setNote(s, " jnp q ");
  const out = press(s, "down", "enter");
  assert.deepEqual(out.submit, { key: "engine", value: "mysql", label: "MySQL", note: "jnp q" });
  assert.equal(press(s, "tab").state.focus, "list");
});

test("n and p cycle decisions and start each afresh", () => {
  let s = init([engine, auth]);
  s = setNote(press(s, "k").state, "x");
  s = press(s, "n").state;
  assert.equal(decision(s).key, "auth");
  assert.equal(s.choice, 0);
  assert.equal(s.note, "");
  s = press(s, "n").state;
  assert.equal(decision(s).key, "engine");
  assert.equal(s.choice, 1);
  assert.equal(decision(press(s, "p").state).key, "auth");
  assert.equal(hints(s), "⏎ answer · tab note · n/p next · esc cancel");
  assert.equal(hints(init([name])), "⏎ answer · tab options · esc cancel");
});

test("other… answers with the typed text", () => {
  let s = init([engine]);
  let out = press(s, "down", "down", "down");
  assert.equal(onOther(out.state), true);
  out = press(out.state, "enter");
  assert.equal(out.submit, undefined);
  assert.equal(out.state.focus, "note");
  out = press(setNote(out.state, " duckdb "), "enter");
  assert.deepEqual(out.submit, { key: "engine", value: "duckdb", label: "duckdb", note: null });
});

test("free text only starts in the note", () => {
  const s = init([name]);
  assert.equal(s.focus, "note");
  assert.equal(onOther(s), true);
  assert.deepEqual(press(setNote(s, "Larkspur"), "enter").submit, { key: "name", value: "Larkspur", label: "Larkspur", note: null });
});

test("escape and ctrl-c cancel", () => {
  const s = init([engine]);
  assert.equal(press(s, "escape").cancel, true);
  assert.equal(press(s, { key: "c", modifiers: { control: true } }).cancel, true);
  assert.equal(press(s, "x").handled, false);
});

test("bullets in about hang under their text", () => {
  assert.deepEqual(aboutBlocks("Pick one.\n- one two\n  - nested\n-not a bullet\n"), [
    { bullet: false, indent: 0, text: "Pick one." },
    { bullet: true, indent: 0, text: "one two" },
    { bullet: true, indent: 2, text: "nested" },
    { bullet: false, indent: 0, text: "-not a bullet" },
    { bullet: false, indent: 0, text: "" },
  ]);
  assert.deepEqual(aboutBlocks(""), [{ bullet: false, indent: 0, text: "" }]);
});
