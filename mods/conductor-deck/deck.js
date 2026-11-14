// The deck: one card per task in the space's repo, read from `conductor deck --json` and
// re-read whenever conductor's state for the repo changes, mixed with the workers' live status.
import { div, svg } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, ScrollHandle } from "gpui-base";
import {
  agents, focused, hc, nextCommand, openOverlay, panes, props, run, spaces, terminals,
  theme, watchFolder,
} from "herdr";
import {
  BLOCKED, feedArgv, gone, lastLine, mergeLastGood, messageResult, parseFeed, problem,
  prProblem, sendArgv,
} from "./feed.js";
import { conductor } from "./conductor.js";
import { SECTION_LABEL, buildCards, elapsed, follow, moveBy, nextChangeIn, prLine } from "./model.js";

const MOD = "conductor-deck";
const ICONS = `mods/${MOD}/icons`;
/** How long the deck waits for a burst of state changes to settle before re-reading. */
const DEBOUNCE = 150;
/** How long a toast shows. */
const TOAST = 4000;
/** How often to retry watching a state dir that isn't there yet. */
const RETRY = 5000;

/**
 * Duplicate deck panes some copy has already closed. Module scope is shared by every copy of
 * the mod, so only one of them closes each pane; a second close would fail and say so.
 */
const CLOSING = new Set();

const icon = (name, size = 14) => svg(`${ICONS}/${name}.svg`).size(size).flex_none();

export class Deck {
  init(cx) {
    this.h = cx.focus_handle();
    this.scroll = ScrollHandle.new();
    this.input = InputState.new({ placeholder: "Message the worker" });
    this.input.on("submit", (_e, cx) => this.send(cx));

    this.spaceId = undefined;   // the space shown; undefined until the first render picks one
    this.space = null;
    this.status = { kind: "loading" }; // loading | ok | none (no conductor) | remote | nospace
    this.repo = null;
    this.tasks = [];
    this.problem = null;

    this.selId = null;
    this.selIndex = 0;
    this.showHidden = false;
    this.foldDone = false;
    this.composing = null;      // { id, terminal }
    this.toastText = null;
    this.toastTask = null;
    this.childOf = new Map();   // card id -> index among the body's children, from the last render

    this.inflight = false;
    this.dirty = false;
    this.debounce = null;
    this.tick = null;
    this.watchedDir = null;
    this.watchGen = 0;

    cx.spawn(async (cx) => {
      for (;;) {
        const command = await nextCommand();
        // pane.go onto the deck, or a popup over it closing.
        if (command === "focus") this.h.focus();
        cx.notify();
      }
    });
  }

  // ---- data --------------------------------------------------------------------------------

  /** The space to show: the one the deck was opened for, else the focused one. */
  wantedSpace() {
    return props()?.space ?? focused()?.space ?? null;
  }

  resolve(cx) {
    const space = spaces().find((s) => s.id === this.spaceId) ?? null;
    this.space = space;
    this.repo = null;
    this.tasks = [];
    this.problem = null;
    this.watchGen++;
    this.watchedDir = null;
    if (!space) this.status = { kind: "nospace" };
    else if (space.machine) this.status = { kind: "remote" };
    else if (!space.folder) this.status = { kind: "none", reason: "the space has no folder" };
    else {
      this.status = { kind: "loading" };
      this.fetch(cx);
    }
    cx.notify();
  }

  /** Re-reads the feed once a burst of changes settles. */
  schedule(cx) {
    this.debounce?.cancel();
    this.debounce = cx.timer.after(DEBOUNCE, (cx) => this.fetch(cx));
  }

  fetch(cx) {
    if (this.inflight) {
      this.dirty = true;
      return;
    }
    const space = this.space;
    if (!space?.folder) return;
    this.inflight = true;
    cx.spawn(async (cx) => {
      let result;
      try {
        result = await conductor(feedArgv(), space.folder);
      } catch (e) {
        result = { code: -1, stdout: "", stderr: String(e?.message ?? e) };
      }
      this.inflight = false;
      if (space === this.space) this.apply(result, cx);
      if (this.dirty) {
        this.dirty = false;
        this.fetch(cx);
      }
      this.rearm(cx);
      cx.notify();
    });
  }

  apply(result, cx) {
    if (result.code !== 0) {
      this.status = { kind: "none", reason: lastLine(result.stderr) ?? `conductor exited with ${result.code}` };
      return;
    }
    let feed;
    try {
      feed = parseFeed(result.stdout);
    } catch (e) {
      this.status = { kind: "none", reason: `unexpected \`conductor deck\` output: ${e?.message ?? e}` };
      return;
    }
    this.status = { kind: "ok" };
    this.repo = feed.repo;
    this.tasks = mergeLastGood(this.tasks, feed);
    this.problem = problem(feed.errors);
    this.watch(feed.repo.dir, cx);
  }

  /** Re-reads the feed whenever anything under the repo's state dir changes. */
  watch(dir, cx) {
    if (!dir || this.watchedDir === dir) return;
    this.watchedDir = dir;
    const gen = ++this.watchGen;
    cx.spawn(async (cx) => {
      for (;;) {
        try {
          await watchFolder(dir);
        } catch {
          // Most likely not created yet: a repo with no tasks. Look again in a while.
          await cx.sleep(RETRY);
        }
        if (gen !== this.watchGen) return;
        this.schedule(cx);
      }
    });
  }

  /** Redraws when the next shown elapsed time changes, and no more often. */
  rearm(cx) {
    this.tick?.cancel();
    this.tick = null;
    if (this.status.kind !== "ok") return;
    const wait = nextChangeIn(this.cards().cards, Date.now());
    if (wait === null) return;
    this.tick = cx.timer.after(wait + 50, (cx) => {
      this.tick = null;
      cx.notify();
      this.rearm(cx);
    });
  }

  /** Each terminal's status as the app reports it. */
  statuses() {
    const map = new Map();
    for (const t of terminals() ?? []) if (t?.address && t.status) map.set(t.address, t.status);
    for (const a of agents() ?? []) map.set(a.address, a.status);
    return map;
  }

  cards() {
    return buildCards(this.tasks, this.statuses(), { showHidden: this.showHidden, foldDone: this.foldDone });
  }

  /** Whether the app still has terminal `address`. */
  live(address) {
    if (!address) return false;
    if ((agents() ?? []).some((a) => a.address === address)) return true;
    return (terminals() ?? []).some((t) => t?.address === address && t.state !== "finished");
  }

  // ---- actions -----------------------------------------------------------------------------

  toast(text, cx) {
    this.toastText = text;
    this.toastTask?.cancel();
    this.toastTask = cx.timer.after(TOAST, (cx) => {
      this.toastText = null;
      this.toastTask = null;
      cx.notify();
    });
    cx.notify();
  }

  selected() {
    const { cards } = this.cards();
    const i = follow(this.selId, this.selIndex, cards);
    return i < 0 ? null : cards[i];
  }

  select(id) {
    const { cards } = this.cards();
    const i = cards.findIndex((c) => c.id === id);
    if (i >= 0) {
      this.selId = id;
      this.selIndex = i;
    }
  }

  move(delta) {
    const { cards } = this.cards();
    const i = moveBy(follow(this.selId, this.selIndex, cards), delta, cards.length);
    if (i < 0) return;
    this.selIndex = i;
    this.selId = cards[i].id;
    const child = this.childOf.get(this.selId);
    if (child !== undefined) this.scroll.scroll_to_item(child);
  }

  focusWorker(card, cx) {
    if (!this.live(card.terminal)) return this.toast(gone(card.id), cx);
    try {
      run("agent.focus", { terminal: card.terminal });
    } catch (e) {
      this.toast(`focus failed: ${e?.message ?? e}`, cx);
    }
  }

  focusConductor(cx) {
    const terminal = this.repo?.terminal;
    if (!terminal) return this.toast("no conductor terminal recorded", cx);
    if (!this.live(terminal)) return this.toast("the conductor's terminal is gone; run `conductor start`", cx);
    try {
      run("agent.focus", { terminal });
    } catch (e) {
      this.toast(`focus failed: ${e?.message ?? e}`, cx);
    }
  }

  compose(card, cx) {
    if (!this.live(card.terminal)) return this.toast(gone(card.id), cx);
    if (this.statuses().get(card.terminal) === "blocked") return this.toast(BLOCKED, cx);
    this.composing = { id: card.id, terminal: card.terminal };
    this.input.set_value("");
    this.input.focus();
    cx.notify();
  }

  endCompose(cx) {
    this.composing = null;
    this.input.set_value("");
    // After this keystroke is done with, so the Enter that sent it doesn't also reach the deck.
    cx.timer.after(1, (cx) => {
      this.h.focus();
      cx.notify();
    });
    cx.notify();
  }

  send(cx) {
    const target = this.composing;
    if (!target) return;
    const text = this.input.value().trim();
    this.endCompose(cx);
    if (!text) return;
    cx.spawn(async (cx) => {
      const result = await hc(sendArgv(target.terminal, text)).catch((e) => ({ code: -1, stderr: String(e?.message ?? e) }));
      this.toast(messageResult(target.id, result), cx);
    });
  }

  decide(card, cx) {
    if (!card.decisions.length) return this.toast(`${card.id}: no open decisions`, cx);
    try {
      openOverlay({
        mod: MOD,
        kind: "dialog",
        modal: true,
        width: 0.7,
        height: 0.7,
        props: {
          view: "decide",
          task: card.id,
          repo: this.repo.id,
          terminal: card.terminal,
          cwd: this.space.folder,
          order: card.decisions.map((d) => d.key),
        },
      });
    } catch (e) {
      this.toast(`couldn't open the decision: ${e?.message ?? e}`, cx);
    }
  }

  openPr(card, cx) {
    const why = prProblem(card.id, card.prUrl);
    if (why) return this.toast(why, cx);
    try {
      cx.open_url(card.prUrl);
    } catch (e) {
      this.toast(`couldn't open the PR: ${e?.message ?? e}`, cx);
    }
  }

  /** Closes this deck's own pane: the deck pane that has the keyboard. */
  quit(cx) {
    const mine = (panes() ?? []).find((p) => p.mod === MOD && p.focused);
    const pane = mine?.pane ?? focused()?.pane;
    if (!pane || !(panes() ?? []).some((p) => p.pane === pane && p.mod === MOD)) {
      return this.toast("this deck isn't a pane; close it from its layout", cx);
    }
    try {
      run("pane.close", { pane });
    } catch (e) {
      this.toast(`couldn't close the deck: ${e?.message ?? e}`, cx);
    }
  }

  /**
   * One deck per tab: conductor's hook opens one whenever a conductor session starts afresh,
   * and can't see whether one is already open. Every copy picks the same panes to close.
   */
  closeDuplicates(cx) {
    const seen = new Set();
    const extra = [];
    for (const p of panes() ?? []) {
      if (p.mod !== MOD) continue;
      if (seen.has(p.tab)) extra.push(p.pane);
      else seen.add(p.tab);
    }
    const todo = extra.filter((pane) => !CLOSING.has(pane));
    if (!todo.length) return;
    for (const pane of todo) CLOSING.add(pane);
    cx.spawn(async () => {
      for (const pane of todo) {
        if (!(panes() ?? []).some((p) => p.pane === pane)) continue;
        try {
          run("pane.close", { pane });
        } catch {}
      }
    });
  }

  onKey(e, cx) {
    if (this.composing) {
      if (e.key === "escape") {
        this.endCompose(cx);
        cx.stop_propagation();
      }
      return;
    }
    // A key meant for something inside the deck, like the message box that just sent.
    if (!this.h.is_focused()) return;
    const m = e.modifiers ?? {};
    if (m.platform || m.alt || (m.control && e.key !== "c")) return;
    const card = this.selected();
    const k = e.key;
    if (k === "q" || (k === "c" && m.control)) this.quit(cx);
    else if (k === "j" || k === "down") this.move(1);
    else if (k === "k" || k === "up") this.move(-1);
    else if (k === "z") this.foldDone = !this.foldDone;
    else if ((k === "l" && m.shift) || k === "L") this.showHidden = !this.showHidden;
    else if (k === "c") this.focusConductor(cx);
    else if (card && k === "enter") this.focusWorker(card, cx);
    else if (card && k === "m") this.compose(card, cx);
    else if (card && k === "d") this.decide(card, cx);
    else if (card && k === "o") this.openPr(card, cx);
    else return;
    cx.stop_propagation();
    if (k === "z" || k === "l" || k === "L") this.rearm(cx);
    cx.notify();
  }

  // ---- drawing -----------------------------------------------------------------------------

  render(cx) {
    const wanted = this.wantedSpace();
    if (wanted !== this.spaceId) {
      this.spaceId = wanted;
      cx.spawn(async (cx) => this.resolve(cx));
    }
    this.closeDuplicates(cx);

    const t = theme();
    const root = v_flex()
      .id("deck")
      .track_focus(this.h)
      .on_key_down((e, cx) => this.onKey(e, cx))
      .on_mouse_down("left", () => {
        if (!this.composing) this.h.focus();
      })
      .size_full()
      .min_h(0)
      .bg(t.background)
      .text_color(t.foreground)
      .text_size(12.5);

    if (this.status.kind !== "ok") return root.child(this.header(t, null)).child(this.empty(t));

    const { cards, counts } = this.cards();
    const index = follow(this.selId, this.selIndex, cards);
    this.selIndex = Math.max(index, 0);
    this.selId = index < 0 ? null : cards[index].id;

    return root
      .child(this.header(t, counts))
      .child(this.body(t, cards, counts))
      .child(this.footer(t, cards, index));
  }

  stateLook(t, c) {
    switch (c.state) {
      case "needs_you": return { icon: "flag", color: t.blocked };
      case "failed": return { icon: "failed", color: t.ansi1 };
      case "in_progress": return c.working ? { icon: "spinner", color: t.working, spin: true } : { icon: "half", color: t.dim };
      case "review": return { icon: "review", color: t.accent };
      case "ready_to_merge": return { icon: "merge", color: t.ansi4 };
      case "pending": return { icon: "pending", color: t.dim };
      case "done":
        if (c.phase === "abandoned") return { icon: "failed", color: t.dim };
        return { icon: "done", color: c.phase === "torn_down" ? t.dim : t.done };
    }
    return { icon: "pending", color: t.dim };
  }

  stateIcon(t, c, size = 13) {
    const look = this.stateLook(t, c);
    const el = icon(look.icon, size).text_color(look.color);
    return look.spin ? el.spin(900) : el;
  }

  header(t, counts) {
    const count = (section, name, color) =>
      h_flex().gap(3).items_center().text_color(counts[section] ? color : t.dim)
        .child(icon(name, 12)).child(div().text_size(11.5).child(String(counts[section])));
    return h_flex()
      .flex_none()
      .h(34)
      .px(12)
      .gap(6)
      .items_center()
      .border_b_1()
      .border_color(t.border)
      .child(icon("terminal", 13).text_color(t.muted))
      .child(div().flex_none().text_size(11.5).font_semibold().child("TASKS"))
      .child(div().flex_1().min_w(0).truncate().text_size(11.5).text_color(t.muted).child(this.repo?.id ?? ""))
      .when(Boolean(counts), (el) => el
        .child(count("needs_you", "flag", t.blocked))
        .child(count("review", "review", t.accent))
        .child(count("in_progress", "spinner", t.working))
        .child(count("todo", "pending", t.muted)));
  }

  empty(t) {
    const box = (title, detail, color = t.foreground) =>
      v_flex().flex_1().min_h(0).items_center().justify_center().gap(6).p(20)
        .child(div().text_size(13).font_semibold().text_color(color).child(title))
        .when(Boolean(detail), (el) => el.child(div().text_size(12).text_color(t.muted).child(detail)));
    switch (this.status.kind) {
      case "loading": return box("Reading tasks…", null, t.muted);
      case "remote": return box("Remote spaces aren't supported yet", "The deck reads conductor's state on this computer.");
      case "nospace": return box("No space", "Open the deck from a conductor's space.");
      default: return box("No conductor here", this.status.reason);
    }
  }

  body(t, cards, counts) {
    const children = [];
    this.childOf = new Map();
    let section = null;
    for (const c of cards) {
      if (c.section !== section) {
        section = c.section;
        children.push(this.rule(t, section, counts[section]));
      }
      this.childOf.set(c.id, children.length);
      children.push(c.section === "done" ? this.doneRow(t, c) : this.card(t, c));
    }
    if (this.foldDone && counts.done) children.push(this.rule(t, "done", counts.done));
    if (!cards.length && !(this.foldDone && counts.done)) {
      children.push(div().pt(24).w_full().text_center().text_color(t.dim).child("no tasks"));
    }
    return v_flex()
      .id("cards")
      .track_scroll(this.scroll)
      .flex_1()
      .min_h(0)
      .overflow_y_scrollbar()
      .px(8)
      .pb(8)
      .gap(6)
      .children(children);
  }

  rule(t, section, count) {
    const needs = section === "needs_you";
    const rule = h_flex()
      .id(`rule-${section}`)
      .pt(8)
      .gap(6)
      .items_center()
      .child(div().flex_none().text_size(10.5).font_semibold().text_color(needs ? t.blocked : t.dim).child(SECTION_LABEL[section]))
      .child(div().flex_none().text_size(10.5).text_color(t.dim).child(String(count)))
      .child(div().flex_1().h(1).bg(t.border));
    if (section !== "done") return rule;
    return rule
      .cursor_pointer()
      .on_click((_e, cx) => {
        this.foldDone = !this.foldDone;
        this.rearm(cx);
        cx.notify();
      })
      .child(icon(this.foldDone ? "chevron-right" : "chevron-down", 12).text_color(t.dim));
  }

  onCard(c) {
    return (e, cx) => {
      this.select(c.id);
      this.h.focus();
      this.focusWorker(c, cx);
      cx.notify();
    };
  }

  card(t, c) {
    const selected = c.id === this.selId;
    const look = this.stateLook(t, c);
    const pr = prLine(c);
    const now = Date.now();
    const line = () => h_flex().gap(6).items_center().min_w(0);
    return v_flex()
      .id(`card-${c.id}`)
      .gap(4)
      .px(10)
      .py(8)
      .rounded(7)
      .border_1()
      .border_color(selected ? t.accent : c.section === "needs_you" ? t.blocked : t.border)
      .bg(selected ? t.selected : t.surface)
      .hover((s) => s.bg(selected ? t.selected : t.hover))
      .cursor_pointer()
      .on_click(this.onCard(c))
      .child(line()
        .child(div().flex_1().min_w(0).truncate().text_size(11).text_color(t.dim).child(c.id))
        .child(h_flex().flex_none().gap(4).items_center().text_size(11).text_color(t.dim)
          .child(icon("clock", 11))
          .child(elapsed(now - c.since))))
      .child(line()
        .child(this.stateIcon(t, c))
        .child(div().flex_none().text_size(12).font_medium().text_color(look.color).child(c.word))
        .when(Boolean(c.detail), (el) => el.child(div().flex_1().min_w(0).truncate().text_size(12).text_color(t.muted).child(`· ${c.detail}`))))
      .child(div()
        .text_size(13)
        .line_height(1.3)
        .max_h(Math.ceil(13 * 1.3 * 3))
        .overflow_hidden()
        .when(selected, (el) => el.font_semibold())
        .child(c.title))
      .when(Boolean(c.branch), (el) => el.child(line().text_size(11.5).text_color(t.muted)
        .child(icon("branch", 12))
        .child(div().flex_1().min_w(0).truncate().child(c.branch))))
      .when(Boolean(pr), (el) => el.child(this.prRow(t, pr)));
  }

  prRow(t, pr) {
    const checks = pr.checks === "pass" ? ["done", t.done] : pr.checks === "fail" ? ["failed", t.ansi1] : ["pending", t.dim];
    const review = pr.reviewTone === "good" ? t.done : pr.reviewTone === "bad" ? t.ansi1 : t.muted;
    return h_flex().gap(6).items_center().min_w(0).text_size(11.5).text_color(t.muted)
      .child(icon("pr", 12))
      .child(div().flex_none().child(pr.state ? `${pr.label} ${pr.state}` : pr.label))
      .when(Boolean(pr.checks), (el) => el.child(h_flex().flex_none().gap(3).items_center()
        .child(div().text_color(t.dim).child("· checks"))
        .child(icon(checks[0], 12).text_color(checks[1]))))
      .when(Boolean(pr.review), (el) => el.child(div().flex_1().min_w(0).truncate().text_color(review).child(`· ${pr.review}`)));
  }

  doneRow(t, c) {
    const selected = c.id === this.selId;
    return h_flex()
      .id(`card-${c.id}`)
      .gap(6)
      .px(10)
      .py(4)
      .items_center()
      .rounded(5)
      .bg(selected ? t.selected : t.background)
      .hover((s) => s.bg(selected ? t.selected : t.hover))
      .cursor_pointer()
      .on_click(this.onCard(c))
      .text_size(11.5)
      .text_color(selected ? t.foreground : t.dim)
      .child(this.stateIcon(t, c, 12))
      .child(div().flex_none().child(c.id))
      .child(div().flex_1().min_w(0).truncate().child(c.title))
      .child(div().flex_none().child(elapsed(Date.now() - c.since)));
  }

  footer(t, cards, index) {
    const hints = [
      "⏎ focus", "m msg", "d decide", "o PR", "c conductor",
      this.foldDone ? "z show done" : "z fold done",
      this.showHidden ? "L hide done" : "L show all",
      "q close",
    ].join(" · ");
    return v_flex()
      .flex_none()
      .px(12)
      .py(6)
      .gap(5)
      .border_t_1()
      .border_color(t.border)
      .when(Boolean(this.problem), (el) => el.child(div().text_size(11.5).text_color(t.ansi1).child(`! ${this.problem}`)))
      .when(Boolean(this.toastText), (el) => el.child(div().text_size(11.5).text_color(t.accent).child(this.toastText)))
      .when(Boolean(this.composing), (el) => el.child(h_flex().gap(6).items_center()
        .child(div().flex_none().text_size(11.5).text_color(t.muted).child(`› ${this.composing.id}`))
        .child(Input.new(this.input).flex_1().h(28).px(8).rounded(6).border_1().border_color(t.accent)
          .bg(t.surface).text_size(12.5).text_color(t.foreground))))
      .child(h_flex().gap(8).items_center().text_size(10.5).text_color(t.dim)
        .child(div().flex_1().min_w(0).truncate().child(this.composing ? "⏎ send · esc cancel" : hints))
        .when(index >= 0, (el) => el.child(div().flex_none().child(`${index + 1}/${cards.length}`))));
  }
}
