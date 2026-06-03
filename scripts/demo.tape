# vhs tape file for the fj README demo.
#
# Reproduce with:
#   brew install vhs
#   ./scripts/record-demo.sh
#
# The tape is the source of truth. Edit it, re-record, commit the
# updated assets/demo.gif and (optionally) assets/demo.mp4.
#
# Pre-requisites the recorder needs:
#   * `fj` on PATH (build it via `cargo build --release` and symlink)
#   * `fj auth login --host rasterhub.com` already completed
#   * Run from inside the fj clone (so `fj repo view` resolves via the
#     git remote)

Output assets/demo.gif
Output assets/demo.mp4

Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1100
Set Height 720
Set TypingSpeed 50ms
Set Padding 24
Set PlaybackSpeed 1.0
Set Shell zsh

# Establish a clean prompt. Enable `#` comments and disable the pager so
# short command output prints inline instead of parking in a pager.
Hide
Type "setopt interactive_comments"
Enter
Type "export FJ_PAGER=cat PAGER=cat"
Enter
Type "clear"
Enter
Show
Sleep 600ms

# 1. The hook: inside a clone, fj infers the repo AND host from the remote.
#    No -R, no host flag, no config. This is the thing gh users miss most.
Type "# inside a clone, fj infers the repo + host from your git remote"
Sleep 200ms
Enter
Sleep 500ms

Type "fj repo view"
Sleep 200ms
Enter
Sleep 2600ms

# 2. The daily driver: the PR loop.
Type "# the everyday PR loop: list, inspect, check CI"
Sleep 200ms
Enter
Sleep 500ms

Type "fj pr list --state all -L 5"
Sleep 200ms
Enter
Sleep 2400ms

Type "fj pr checks 63"
Sleep 200ms
Enter
Sleep 2400ms

# 3. Forgejo Actions from the terminal: runs and their status.
Type "# Forgejo Actions, without leaving the terminal"
Sleep 200ms
Enter
Sleep 500ms

Type "fj run list -L 6"
Sleep 200ms
Enter
Sleep 2600ms

# 4. Scriptable: gh-style selective JSON projection over any --json output.
Type "# gh-style selective JSON for scripts and agents"
Sleep 200ms
Enter
Sleep 500ms

Type "fj pr list --state all -L 3 --json --json-fields number,title,state"
Sleep 200ms
Enter
Sleep 3200ms
