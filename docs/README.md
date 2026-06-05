# `fj` documentation

- [`architecture.md`](architecture.md): module graph, layering rules,
  the HTTP funnel, pager + SIGINT, repo resolution, test strategy.
- [`stacks.md`](stacks.md): stacked branches and PRs (new, sync, ship,
  split, absorb).
- [`agent.md`](agent.md): AI-assisted code ops (review, fix, explain).
- [`jq.md`](jq.md): the `fj api --jq` syntax. Dot paths, brackets,
  negative indices, pipes.
- [`gh-to-fj.md`](gh-to-fj.md): command-by-command mapping from gh.
- [`compatibility.md`](compatibility.md): Forgejo version matrix and
  known caveats on older Gitea.
- [`faq.md`](faq.md): common questions about tokens, hosts, debug,
  scripting, plugins.
- [`troubleshooting.md`](troubleshooting.md): keychain prompts, hangs,
  401 errors, `--debug`, pager opt-out, alias precedence, hook bypass.

For build/test workflow see [`../CONTRIBUTING.md`](../CONTRIBUTING.md).
For project conventions see [`../CLAUDE.md`](../CLAUDE.md).
For security policy see [`../SECURITY.md`](../SECURITY.md).
For release notes see [`../CHANGELOG.md`](../CHANGELOG.md).
