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

# Establish a clean prompt.
Hide
Type "clear"
Enter
Show
Sleep 600ms

Type "fj --version"
Sleep 300ms
Enter
Sleep 1200ms

Type "# inside a clone, fj infers the repo from the git remote"
Sleep 200ms
Enter
Sleep 600ms

Type "fj repo view | head -10"
Sleep 200ms
Enter
Sleep 2200ms

Type "# issues, PRs, releases — same shape"
Sleep 200ms
Enter
Sleep 500ms

Type "fj issue list --state all -L 5"
Sleep 200ms
Enter
Sleep 2000ms

Type "fj pr list --state all -L 5"
Sleep 200ms
Enter
Sleep 2200ms

Type "# api escape hatch with jq-ish projection"
Sleep 200ms
Enter
Sleep 500ms

Type "fj api /version"
Sleep 200ms
Enter
Sleep 1500ms

Type "fj api /user -q .login"
Sleep 200ms
Enter
Sleep 1500ms

Type "# gh-style selective JSON for scripts"
Sleep 200ms
Enter
Sleep 500ms

Type "fj repo list -L 3 --json --json-fields full_name,private"
Sleep 200ms
Enter
Sleep 2800ms

Type "fj --help | head -20"
Sleep 200ms
Enter
Sleep 3000ms
