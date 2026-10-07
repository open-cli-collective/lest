# Agent guide

Repository facts, layout, commands and invariants: [docs/development.md](docs/development.md).
Why Lest is shaped the way it is: [docs/design.md](docs/design.md).
The flow format: [docs/flow-format.md](docs/flow-format.md).

Shared standards for Open CLI Collective repositories:

```md
Source of truth: https://github.com/open-cli-collective/cli-common/blob/main/docs/README.md
Local convenience copy, if present: `../cli-common/docs/README.md`
```

Shared automation:

```md
Source of truth: https://github.com/open-cli-collective/.github
Local convenience copy, if present: `../.github`
```

Before pushing, run `make check`. Keep everything company-neutral: examples use
the bundled sample app or `example.com` placeholders.
