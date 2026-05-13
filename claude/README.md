# fj Claude Code plugin

A [Claude Code](https://docs.claude.com/en/docs/claude-code) plugin that
teaches Claude how to use `fj`, the CLI for Forgejo and Gitea-compatible
instances.

The plugin ships a single skill (`fj`) that activates when the user
mentions fj, Forgejo, Gitea, or any forge-side action (open a PR, list
issues, cut a release, request a review, etc.) on a non-GitHub host.

## Install

This directory is itself a complete Claude Code plugin. Two ways to
install:

```sh
# From a local clone of the upstream fj repo:
/plugin install /path/to/fj/claude

# From the canonical mirror repo (preferred for users who aren't
# already cloning fj for development):
/plugin install rasterandstate/fj-claude-plugin
```

After install, the skill activates automatically when relevant.

## Layout

```
claude/
├── .claude-plugin/
│   └── plugin.json       plugin manifest (name, version, keywords)
├── README.md             this file
└── skills/
    └── fj/
        └── SKILL.md      the skill body — what Claude reads
```

## Updating

This `claude/` directory is the source of truth, inside the fj repo at
[rasterhub.com/rasterstate/fj](https://rasterhub.com/rasterstate/fj).

A mirror lives at
[github.com/rasterandstate/fj-claude-plugin](https://github.com/rasterandstate/fj-claude-plugin),
synced from this directory. To cut a new version:

1. Bump `version` in `.claude-plugin/plugin.json` here.
2. Update SKILL.md if new fj commands or workflows exist.
3. Commit + push the fj repo.
4. Sync the contents of `claude/` over to the mirror repo and tag it:

   ```sh
   # From the fj repo root:
   rsync -av --delete --exclude=.git claude/ /path/to/fj-claude-plugin/
   cd /path/to/fj-claude-plugin
   git commit -am "sync from fj vX.Y.Z"
   git tag vX.Y.Z
   git push origin main vX.Y.Z
   ```

## License

MIT, matching fj.
