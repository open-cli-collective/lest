# AI (optional)

Lest works the same with or without AI. It is off by default, nothing is
sent anywhere until you choose a provider, and turning it on or off changes
no layout: AI only fills places that already have content, and AI actions
live in menus.

## What it does

- **Explains failed steps.** Every failed step already has a deterministic
  headline built from its own output (the exit code and the error line, the
  HTTP status and the body's message, the expectation and what the page
  showed). With a provider, the failure card and `lest explain` add a short
  explanation written by the model, marked as such, with the headline kept
  beside it. The model gets the redacted report, the step's output tail, its
  notes and the flow file, and answers once per step; the answer is cached.
- **Hands off to your own agent.** The failure card's menu and `lest context`
  give everything needed to pick up a failure: the flow and report paths, the
  failing step, its output and the rerun command. With an agent configured,
  **Open in agent** starts your agent CLI in a terminal, in the project, with
  that task. Lest has no chat of its own: your agent already has tools,
  permissions and memory.
- **Agent-readable docs.** `lest docs agent` prints the authoring guide and
  the format references for a coding agent to read on demand. This needs no
  provider.

A model never decides whether a run passed, and a run never waits for a model.

## Choosing a provider

In the UI: Settings, AI. From the CLI:

```bash
lest config set ai.provider auto        # the first agent CLI found on PATH
lest config set ai.provider command     # any program: prompt on stdin, text on stdout
lest config set ai.command 'my-llm --quiet'
lest config set ai.model small-model    # optional, passed to agent CLIs
lest config set ai.agent 'my-agent {prompt}'   # what Open in agent runs
lest config set ai.dailyLimit 50
lest config show
```

| Provider | Uses |
|---|---|
| `none` | No AI (default). |
| `auto` | The first supported agent CLI on PATH, else none. |
| a supported agent CLI | That CLI, headless: one answer, no tools, no MCP servers, no user customizations, in an empty temporary directory, killed after 90 seconds. |
| `command` | Any program that reads the prompt on stdin and prints the answer. Labeled by the program's name. |

Settings live in `config.yml` in Lest's config directory (`lest config show`
prints the path). Calls are counted per day in the data directory; past
`dailyLimit`, explanations fall back to the headline with a note. Answers are
cached by input, so reopening a failure costs nothing.

## Without AI

Everything else is identical: the same failure card with the deterministic
headline, the same Copy context and rerun actions, the same CLI. Only the
menu items that need an agent are absent.
