// conductor-deck: conductor's tasks as cards beside the conductor.
//
// As a pane (opened by conductor's SessionStart hook, or a key): props { space? }. Without a
// space it follows the focused one, so it also works as a block in a layout.
// As the decision dialog it opens over itself: props { view: "decide", task, repo, terminal?,
// cwd, order? }.
import { View } from "gpui-kit";
import { props } from "herdr";
import { Deck } from "./deck.js";
import { Decide } from "./decide.js";

export default class ConductorDeck extends View {
  init(_props, cx) {
    this.view = props()?.view === "decide" ? new Decide() : new Deck();
    this.view.init(cx);
  }

  render(cx) {
    return this.view.render(cx);
  }
}
