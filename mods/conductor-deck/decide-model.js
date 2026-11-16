// The decision dialog's state: which open decision is shown, which option is chosen, and
// whether the keyboard is on the options or the note. A port of the v0.3.0 deck's
// `ui/decision.rs`. Typing goes to a GPUI Input; this only sees the note through `setNote`.
// Pure: no herdr imports, so `node --test` covers it.

/** The free-text row's label, after the options. */
export const OTHER = "other…";

/** The dialog over `decisions` (oldest first), or null with none to answer. */
export function init(decisions) {
  if (!decisions?.length) return null;
  return reset({ decisions, index: 0, choice: 0, focus: "list", note: "" });
}

/** The shown decision afresh: on its recommendation, the note empty. */
function reset(s) {
  const d = s.decisions[s.index];
  const rec = d.recommend === null ? -1 : d.options.findIndex((o) => o.value === d.recommend);
  return { ...s, choice: Math.max(rec, 0), focus: d.options.length ? "list" : "note", note: "" };
}

export const decision = (s) => s.decisions[s.index];

/** The rows: the options, then `other…` if free text is allowed. */
export const rows = (s) => decision(s).options.length + (decision(s).allowFreeform ? 1 : 0);

/** Whether the `other…` row is chosen, so the note is the answer itself. */
export const onOther = (s) => s.choice >= decision(s).options.length;

export const setNote = (s, note) => ({ ...s, note });

const moveBy = (s, delta) => ({ ...s, choice: Math.min(Math.max(s.choice + delta, 0), Math.max(rows(s) - 1, 0)) });

/** The next decision, or the previous one, wrapping around. */
function cycle(s, forward) {
  const n = s.decisions.length;
  return reset({ ...s, index: (s.index + (forward ? 1 : n - 1)) % n });
}

/**
 * The answer as chosen: `{ key, value, label, note }`. With `other…` chosen and nothing typed,
 * there's none yet, and the focus moves to the note instead.
 */
export function submit(s) {
  const d = decision(s);
  const text = s.note.trim();
  const option = d.options[s.choice];
  if (option) return { state: s, submit: { key: d.key, value: option.value, label: option.label, note: text || null } };
  if (d.allowFreeform && text) return { state: s, submit: { key: d.key, value: text, label: text, note: null } };
  return { state: { ...s, focus: "note" } };
}

/**
 * Handles a key (GPUI's `KeyEvent.key`, plus its modifiers), returning the new state and
 * `submit` or `cancel` when the key ends the dialog. `handled` is false for keys the note
 * should get instead.
 */
export function key(s, { key, modifiers = {} }) {
  if (key === "escape" || (key === "c" && modifiers.control)) return { state: s, cancel: true, handled: true };
  if (key === "enter") return { ...submit(s), handled: true };
  if (key === "up") return { state: moveBy(s, -1), handled: true };
  if (key === "down") return { state: moveBy(s, 1), handled: true };
  if (key === "tab") return { state: { ...s, focus: s.focus === "list" ? "note" : "list" }, handled: true };
  if (s.focus === "note") return { state: s, handled: false };
  if (modifiers.control || modifiers.alt || modifiers.platform) return { state: s, handled: false };
  switch (key) {
    case "k": return { state: moveBy(s, -1), handled: true };
    case "j": return { state: moveBy(s, 1), handled: true };
    case "n": return { state: cycle(s, true), handled: true };
    case "p": return { state: cycle(s, false), handled: true };
  }
  return { state: s, handled: false };
}

/** The dialog's hint line. */
export function hints(s) {
  const parts = ["⏎ answer", s.focus === "note" ? "tab options" : "tab note"];
  if (s.decisions.length > 1) parts.push("n/p next");
  parts.push("esc cancel");
  return parts.join(" · ");
}

/**
 * `about` as lines to draw. A `- ` line is a bullet, drawn with its text in a column of its own
 * so wrapped lines hang under the text, not the dash.
 */
export function aboutBlocks(about) {
  return String(about ?? "")
    .split("\n")
    .map((line) => {
      const m = /^(\s*)- (.*)$/.exec(line);
      return m ? { bullet: true, indent: m[1].length, text: m[2] } : { bullet: false, indent: 0, text: line };
    });
}
