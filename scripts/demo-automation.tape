# vhs tape: automation surface (Actions + JSON + raw API), recorded against
# rasterstate/fj-demo.
#
# Reproduce:
#   ./scripts/seed-demo.sh
#   FJ_DEMO_DIR=/path/to/fj-demo vhs scripts/demo-automation.tape

Output assets/demo-automation.gif

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

Type "# Forgejo Actions, without leaving the terminal"
Sleep 200ms
Enter
Sleep 400ms
Type "fj run list -L 6"
Sleep 200ms
Enter
Sleep 2800ms

Type "# JSON projection for scripts and agents"
Sleep 200ms
Enter
Sleep 400ms
Type "fj pr list --state all --json --json-fields number,title,user.login"
Sleep 200ms
Enter
Sleep 3000ms

Type "# raw API with a jq-ish path"
Sleep 200ms
Enter
Sleep 400ms
Type "fj api /repos/rasterstate/fj-demo/issues -q '.[].title'"
Sleep 200ms
Enter
Sleep 3000ms
