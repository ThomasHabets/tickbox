# tickbox

A simple workflow executor, for presubmits and similar things.

See a simple demo here:
https://asciinema.org/a/VPzajFCTE3Wk1LvDqDavZrZst

## Setting up

1. Create a directory to keep the workflow. For example `tickbox` in your source
   code repository.
2. Create scripts inside this directory. They will be executed in alphabetical
   order, so name them accordingly. E.g. `10-setup.sh`, `20-test.sh`.
4. Make all scripts executable. E.g. `chmod +x tickbox/pre-commit/*.sh`.
5. Optionally, create a `tickbox.json` file with local settings. See below.
6. Test your workflow. `tickbox --dir tickbox/pre-commit --wait`. The
   `--wait` prevents tickbox disappearing if everything succeeded, so that you
   can look around a bit.
7. If this is a git pre-commit hook, then tell git to use it:
   ```
   $ cat > .git/hooks/pre-commit
   set -euo pipefail
   ROOT_DIR="$(pwd)"
   exec tickbox --dir "$ROOT_DIR/extra/pre-commit/" --cwd "$ROOT_DIR"
   ^D
   $ chmod +x .git/hooks/pre-commit
   ```
8. Add the files to git and commit. Tickbox should run on commit.

## Examples

See this repository, as well as:
* https://github.com/ThomasHabets/rustradio

## Config

The `tickbox.json` config file has a few settings that will apply to all scripts
in the workflow. Here's an example config:

```
{
    "envs": {
        "RUSTFLAGS": "--deny warnings",
        "CARGO_TERM_COLOR": "always"
    }
}
```

## Using the UI

The UI has two main parts: The top part shows all the steps in the workflow, and
how they're going. The bottom part shows the output of all steps.

### UI controls

* `j` / Down — Scroll down by one line.
* `k` / Up — Scroll up by one line.
* PageDown — Scroll down by about a page.
* PageUp — Scroll up by about a page.
* `q` — Exit, whether the workflow has completed or not.
* `l` — Redraw the screen, in case it got some ugly garbage.

## Not yet implemented

* Color output is a bit buggy, and requires `l` key sometimes.
* Split step output buffers
* Allow retrying a step.
* Allow skip failing test and continue.
* Render CPU graph while running.
* Log step times, and present on the next run.
* Have a good story for if tickbox triggers another tickbox. This could happen
  if a "prep a new release" workflow triggers a "pre-commit" workflow.
