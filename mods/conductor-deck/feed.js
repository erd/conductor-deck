// What the deck reads from and runs: `conductor deck --json` and `conductor decision …`, and
// `hc send` to the workers. Pure: no herdr imports, so `node --test` covers it.

/** hc's exit codes (herdr-core): no such terminal, waiting on a person, program ended. */
export const HC_NOT_FOUND = 3;
export const HC_BLOCKED = 9;
export const HC_FINISHED = 11;

export const BLOCKED = "worker is waiting on a prompt; press Enter to focus it";
export const gone = (id) => `${id}: worker is gone; run \`conductor resume\``;

const ms = (s) => {
  if (s === undefined || s === null) return null;
  const t = Date.parse(s);
  return Number.isNaN(t) ? null : t;
};

/** The feed's timestamps as ms since the epoch, and every optional field present (or null). */
function task(raw) {
  const report = raw.report
    ? { verb: raw.report.verb, text: raw.report.text ?? null, prUrl: raw.report.pr_url ?? null, ts: ms(raw.report.ts) }
    : null;
  const pr = raw.pr
    ? { state: raw.pr.state, checks: raw.pr.checks ?? null, review: raw.pr.review ?? null, ts: ms(raw.pr.ts) }
    : null;
  return {
    id: raw.id,
    title: raw.title ?? raw.id,
    kind: raw.kind ?? null,
    phase: raw.phase,
    phaseSince: ms(raw.phase_since),
    activity: raw.activity ?? "not_started",
    detail: raw.detail ?? null,
    since: ms(raw.since),
    report,
    decisions: (raw.decisions ?? []).map((d) => ({ key: d.key, title: d.title ?? null, openedAt: ms(d.opened_at) })),
    branch: raw.branch ?? null,
    prUrl: raw.pr_url ?? null,
    pr,
    terminal: raw.terminal_id ?? null,
    updatedAt: ms(raw.updated_at),
  };
}

/**
 * Parses `conductor deck --json`. Tasks that conductor couldn't read become `errors`
 * (`task <id>: <error>`) and are left out of `tasks`. Throws on output that isn't a feed.
 */
export function parseFeed(stdout) {
  const doc = JSON.parse(stdout);
  if (!doc || typeof doc !== "object" || !doc.repo || !Array.isArray(doc.tasks)) {
    throw new Error("not a conductor deck feed");
  }
  const repo = {
    id: doc.repo.id,
    root: doc.repo.root,
    dir: doc.repo.dir,
    terminal: doc.repo.terminal_id ?? null,
    space: doc.repo.space_id ?? null,
  };
  const tasks = [];
  const errors = [];
  for (const raw of doc.tasks) {
    if (typeof raw?.error === "string") errors.push({ id: raw.id, error: raw.error });
    else tasks.push(task(raw));
  }
  return { repo, tasks, errors };
}

/**
 * The tasks to show: `next`'s, plus the last good entry of each task that now fails to read, so
 * a half-written file doesn't make its card vanish.
 */
export function mergeLastGood(prevTasks, next) {
  const prev = new Map((prevTasks ?? []).map((t) => [t.id, t]));
  const kept = next.errors.map((e) => prev.get(e.id)).filter(Boolean);
  return [...next.tasks, ...kept];
}

/** The first read error, as the deck's problem line shows it. */
export function problem(errors) {
  if (!errors.length) return null;
  const first = [...errors].sort((a, b) => cmp(a.id, b.id))[0];
  return `task ${first.id}: ${first.error}`;
}

const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

/** The last line of a failed command's stderr that says anything. */
export function lastLine(stderr) {
  const lines = String(stderr ?? "").split("\n").map((l) => l.trim()).filter(Boolean);
  return lines.length ? lines[lines.length - 1] : null;
}

// ---- argv ----------------------------------------------------------------------------------

export const feedArgv = () => ["deck", "--json"];

export const decisionListArgv = (task, repo) =>
  ["decision", "list", "--open", "--json", "--task", task, "--repo", repo];

export function answerArgv({ key, value, note, task, repo }) {
  const argv = ["decision", "answer", key, value];
  if (note) argv.push("--note", note);
  argv.push("--by", "deck", "--task", task, "--repo", repo);
  return argv;
}

export const sendArgv = (terminal, text) => ["send", terminal, text];

/** What the deck types into a worker once its decision is answered. */
export const continuePrompt = (key, label) =>
  `Decision ${key} answered: ${label}. Check \`conductor decision list\` and continue.`;

/** `sent`, `blocked`, `gone` or `error`, from `hc send`'s exit code. */
export function sendOutcome(code) {
  if (code === 0) return "sent";
  if (code === HC_BLOCKED) return "blocked";
  if (code === HC_NOT_FOUND || code === HC_FINISHED) return "gone";
  return "error";
}

/** The deck's toast after messaging task `id`'s worker. */
export function messageResult(id, result) {
  switch (sendOutcome(result.code)) {
    case "sent": return `sent to ${id}`;
    case "blocked": return BLOCKED;
    case "gone": return gone(id);
    default: return `message failed: ${lastLine(result.stderr) ?? `hc exited with ${result.code}`}`;
  }
}

/** Why `conductor decision answer` failed. */
export function answerFailed(result) {
  return `answer failed: ${lastLine(result.stderr) ?? `conductor exited with ${result.code}`}`;
}

/** What the dialog says once the answer is recorded, given how telling the worker went. */
export function answeredResult(id, key, result) {
  switch (result ? sendOutcome(result.code) : "gone") {
    case "sent": return { told: true, message: `answered ${key} for ${id}` };
    case "blocked": return { told: false, message: `answered ${key}; worker is waiting on a prompt, so it wasn't told` };
    case "gone": return { told: false, message: `answered ${key}; ${gone(id)}` };
    default: return {
      told: false,
      message: `answered ${key}, but couldn't tell the worker: ${lastLine(result.stderr) ?? `hc exited with ${result.code}`}`,
    };
  }
}

/** Parses `conductor decision list --open --json`: still-open decisions only. */
export function parseDecisions(stdout) {
  const list = JSON.parse(stdout);
  if (!Array.isArray(list)) throw new Error("not a decision list");
  return list
    .filter((d) => !d.answer)
    .map((d) => ({
      task: d.task,
      key: d.key,
      title: d.title ?? d.key,
      about: d.about ?? "",
      options: (d.options ?? []).map((o) => ({ value: o.value, label: o.label, hint: o.hint ?? null })),
      recommend: d.recommend ?? null,
      allowFreeform: Boolean(d.allow_freeform),
    }));
}

// ---- PRs -----------------------------------------------------------------------------------

export const isWebUrl = (url) => /^https?:\/\//.test(url ?? "");

/** The program that opens a URL in the browser, by `window().platform`. */
export const opener = (platform) => (platform === "macos" ? "open" : "xdg-open");

/** Why the PR can't be opened, or null if `url` can be. */
export function prProblem(id, url) {
  if (!url) return `${id}: no PR yet`;
  if (!isWebUrl(url)) return `not a web URL: ${url}`;
  return null;
}
