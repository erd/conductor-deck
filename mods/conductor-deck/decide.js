// The decision dialog, opened over the window by the deck's `d`. Answers with
// `conductor decision answer … --by deck`, then tells the worker to continue.
import { div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input } from "gpui-base";
import { closeOverlay, hc, props, theme } from "herdr";
import { conductor } from "./conductor.js";
import {
  answerArgv, answerFailed, answeredResult, continuePrompt, decisionListArgv, lastLine,
  parseDecisions, sendArgv,
} from "./feed.js";
import * as model from "./decide-model.js";

export class Decide {
  init(cx) {
    this.p = props() ?? {};
    this.state = null;
    this.loading = true;
    this.message = null;
    this.failed = false;
    this.closeOnKey = false;
    this.busy = false;
    this.inputFocused = false;

    this.h = cx.focus_handle();
    this.h.focus();
    this.input = InputState.new({ placeholder: "" });
    this.input.on("change", (_e, cx) => {
      if (this.state) this.state = model.setNote(this.state, this.input.value());
      cx.notify();
    });
    this.input.on("focus", (_e, cx) => {
      this.inputFocused = true;
      if (this.state && this.state.focus !== "note") this.state = { ...this.state, focus: "note" };
      cx.notify();
    });
    this.input.on("blur", (_e, cx) => {
      this.inputFocused = false;
      cx.notify();
    });
    // Enter in the note. It may reach the dialog's own key handler too; `busy` keeps that to
    // one answer.
    this.input.on("submit", (_e, cx) => this.press({ key: "enter", modifiers: {} }, cx));
    // A dialog's first focus can be lost to the window settling.
    cx.timer.after(600, () => this.syncFocus());

    cx.spawn(async (cx) => {
      await this.load();
      this.loading = false;
      this.syncFocus();
      cx.notify();
    });
  }

  async load() {
    const { task, repo, cwd, order } = this.p;
    if (!task || !repo) {
      this.message = "no task to decide on";
      this.closeOnKey = true;
      return;
    }
    let result;
    try {
      result = await conductor(decisionListArgv(task, repo), cwd);
    } catch (e) {
      result = { code: -1, stderr: String(e?.message ?? e) };
    }
    if (result.code !== 0) {
      this.message = `couldn't read the decisions: ${lastLine(result.stderr) ?? `conductor exited with ${result.code}`}`;
      this.closeOnKey = true;
      return;
    }
    let decisions;
    try {
      decisions = parseDecisions(result.stdout);
    } catch (e) {
      this.message = `unexpected \`conductor decision list\` output: ${e?.message ?? e}`;
      this.closeOnKey = true;
      return;
    }
    // Oldest first, as the deck ordered them; the list is in key order.
    const rank = new Map((order ?? []).map((key, i) => [key, i]));
    decisions.sort((a, b) => (rank.get(a.key) ?? Infinity) - (rank.get(b.key) ?? Infinity));
    this.state = model.init(decisions);
    if (!this.state) {
      this.message = `${task}: no open decisions`;
      this.closeOnKey = true;
    }
  }

  close() {
    closeOverlay(this.p.overlay ?? props()?.overlay);
  }

  syncFocus() {
    if (this.state?.focus === "note") this.input.focus();
    else this.h.focus();
  }

  press(e, cx) {
    if (this.closeOnKey) return this.close();
    if (!this.state) {
      if (e.key === "escape") this.close();
      return;
    }
    if (this.busy) return;
    const before = this.state;
    const out = model.key(this.state, e);
    if (!out.handled) return;
    cx.stop_propagation();
    if (out.cancel) return this.close();
    this.state = out.state;
    if (this.state.index !== before.index) this.input.set_value("");
    if (out.submit) this.answer(out.submit, cx);
    if (this.state.focus !== before.focus) this.syncFocus();
    cx.notify();
  }

  choose(i, submit, cx) {
    if (!this.state || this.busy) return;
    this.state = { ...this.state, choice: i };
    if (submit) {
      const out = model.submit(this.state);
      this.state = out.state;
      if (out.submit) this.answer(out.submit, cx);
      this.syncFocus();
    }
    cx.notify();
  }

  answer(answer, cx) {
    const { task, repo, terminal, cwd } = this.p;
    this.busy = true;
    this.failed = false;
    this.message = `answering ${answer.key}…`;
    cx.spawn(async (cx) => {
      const recorded = await conductor(answerArgv({ ...answer, task, repo }), cwd)
        .catch((e) => ({ code: -1, stderr: String(e?.message ?? e) }));
      if (recorded.code !== 0) {
        this.busy = false;
        this.failed = true;
        this.message = answerFailed(recorded);
        cx.notify();
        return;
      }
      const told = terminal
        ? await hc(sendArgv(terminal, continuePrompt(answer.key, answer.label))).catch((e) => ({ code: -1, stderr: String(e?.message ?? e) }))
        : null;
      const result = answeredResult(task, answer.key, told);
      if (result.told) return this.close();
      this.message = result.message;
      this.closeOnKey = true;
      this.h.focus();
      cx.notify();
    });
  }

  render() {
    const t = theme();
    const root = v_flex()
      .id("decide")
      .track_focus(this.h)
      .on_key_down((e, cx) => this.press(e, cx))
      .size_full()
      .min_h(0)
      .p(18)
      .gap(12)
      .bg(t.popover)
      .text_color(t.foreground)
      .text_size(13);

    if (!this.state || this.closeOnKey) {
      return root
        .child(this.top(t))
        .child(v_flex().flex_1().min_h(0).items_center().justify_center().gap(8)
          .child(div().text_color(this.loading ? t.muted : t.foreground).child(this.loading ? "Reading decisions…" : this.message ?? ""))
          .when(!this.loading, (el) => el.child(div().text_size(11.5).text_color(t.dim).child("any key closes"))));
    }

    const s = this.state;
    const d = model.decision(s);
    const other = model.onOther(s);
    return root
      .child(this.top(t))
      .child(div().text_size(16).font_semibold().child(d.title))
      .child(v_flex()
        .id("decide-body")
        .flex_1()
        .min_h(0)
        .overflow_y_scrollbar()
        .gap(12)
        .child(this.about(t, d.about))
        .child(v_flex().gap(2).children([
          ...d.options.map((o, i) => this.option(t, i, o.label, o.hint, o.value === d.recommend)),
          ...(d.allowFreeform ? [this.option(t, d.options.length, model.OTHER, null, false)] : []),
        ])))
      .child(h_flex().gap(8).items_center()
        .child(div().flex_none().w(58).text_size(12).text_color(s.focus === "note" ? t.foreground : t.dim).child(other ? "answer ›" : "note ›"))
        .child(Input.new(this.input).flex_1().h(32).px(10).rounded(7).border_1()
          .border_color(this.inputFocused ? t.accent : t.input).bg(t.background).text_size(12.8).text_color(t.foreground)))
      .when(Boolean(this.message), (el) => el.child(div().text_size(12).text_color(this.failed ? t.ansi1 : t.muted).child(this.message)))
      .child(div().text_size(11).text_color(t.dim).child(model.hints(s)));
  }

  top(t) {
    const s = this.state;
    const key = s ? model.decision(s).key : null;
    return h_flex().items_center().gap(8).text_size(11.5).text_color(t.dim)
      .child(div().flex_1().min_w(0).truncate().child(key ? `${this.p.task}: ${key}` : this.p.task ?? ""))
      .when(Boolean(s && s.decisions.length > 1), (el) => el.child(div().flex_none().child(`${s.index + 1}/${s.decisions.length}`)))
      .child(div().flex_none().child("Esc"));
  }

  about(t, about) {
    const blocks = model.aboutBlocks(about);
    while (blocks.length && !blocks[blocks.length - 1].text) blocks.pop();
    return v_flex().gap(3).text_size(12.5).text_color(t.muted).children(blocks.map((b) =>
      b.bullet
        ? h_flex().items_start().pl(b.indent * 6)
          .child(div().flex_none().w(14).child("•"))
          .child(div().flex_1().min_w(0).child(b.text))
        : b.text ? div().child(b.text) : div().h(4)));
  }

  option(t, i, label, hint, recommended) {
    const s = this.state;
    const chosen = s.choice === i;
    return h_flex()
      .id(`option-${i}`)
      .gap(8)
      .px(10)
      .py(6)
      .items_start()
      .rounded(6)
      .bg(chosen ? t.selected : t.popover)
      .hover((st) => st.bg(chosen ? t.selected : t.hover))
      .cursor_pointer()
      .on_click((e, cx) => this.choose(i, e.click_count >= 2, cx))
      .child(div().flex_none().w(10).text_color(t.accent).font_bold().child(chosen ? "›" : ""))
      .child(v_flex().flex_1().min_w(0).gap(2)
        .child(div().when(chosen, (el) => el.font_semibold()).child(label))
        .when(Boolean(hint), (el) => el.child(div().text_size(11.5).text_color(t.dim).child(hint))))
      .when(recommended, (el) => el.child(div().flex_none().px(6).rounded(4).border_1().border_color(t.accent)
        .text_size(10).font_semibold().text_color(t.accent).child("REC")));
  }
}
