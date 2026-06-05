# vhs tape: the everyday fj loop, recorded against the public demo repo
# rasterstate/fj-demo (NOT fj itself, so the footage isn't self-referential).
#
# Reproduce:
#   brew install vhs
#   ./scripts/seed-demo.sh            # one-time: create + seed rasterstate/fj-demo
#   FJ_DEMO_DIR=/path/to/fj-demo vhs scripts/demo-everyday.tape
#
# Prereqs: `fj` on PATH, `fj auth login --host rasterhub.com` done, and a
# clone of rasterstate/fj-demo at $FJ_DEMO_DIR (default /tmp/fj-demo).

Output assets/demo-everyday.gif

Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1100
Set Height 760
Set TypingSpeed 45ms
Set Padding 24
Set Shell zsh

Hide
Type "setopt interactive_comments"
Enter
Type "export FJ_PAGER=cat PAGER=cat"
Enter
Type "cd ${FJ_DEMO_DIR:-/tmp/fj-demo}"
Enter
Type "clear"
Enter
Show
Sleep 600ms

Type "# inside a clone, fj infers the repo + host from the git remote"
Sleep 200ms
Enter
Sleep 400ms
Type "fj repo view"
Sleep 200ms
Enter
Sleep 2600ms

Type "# what's open right now"
Sleep 200ms
Enter
Sleep 400ms
Type "fj issue list"
Sleep 200ms
Enter
Sleep 2600ms

Type "fj pr list --state all"
Sleep 200ms
Enter
Sleep 3000ms
