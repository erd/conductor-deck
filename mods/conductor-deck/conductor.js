// Runs the conductor CLI. `cargo install` puts it in ~/.cargo/bin, which isn't always on the
// app's PATH, so that's tried when `conductor` isn't found.
import { exec, home } from "herdr";
import { conductorBins } from "./feed.js";

/** `conductor <args>` in `cwd`, as `exec` returns it. Rejects only when no conductor runs. */
export async function conductor(args, cwd) {
  let last;
  for (const bin of conductorBins(home())) {
    try {
      return await exec(bin, args, { cwd });
    } catch (e) {
      last = e;
    }
  }
  throw new Error(`can't run conductor (not on PATH or in ~/.cargo/bin): ${last?.message ?? last}`);
}
