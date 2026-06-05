# vhs tape: stacked PRs, recorded against rasterstate/fj-demo.
#
# Reproduce:
#   ./scripts/seed-demo.sh            # creates the 3-PR "Persistent store" stack
#   FJ_DEMO_DIR=/path/to/fj-demo vhs scripts/demo-stacks.tape
#
# The stack metadata lives in $FJ_DEMO_DIR/.git/fj/stack.json, so this tape
# must run from that exact clone (a fresh clone has no local stack state).

Output assets/demo-stacks.gif

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

Type "# three dependent PRs, tracked locally in .git/fj/stack.json"
Sleep 200ms
Enter
Sleep 400ms
Type "fj stack review"
Sleep 200ms
Enter
Sleep 3200ms

Type "# preview a sync: what would push and update, without doing it"
Sleep 200ms
Enter
Sleep 400ms
Type "fj stack sync --dry-run"
Sleep 200ms
Enter
Sleep 3200ms
