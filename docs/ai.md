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
  beside it. The model gets the step's output tail, its notes, the earlier
  steps' results and the flow file, with every resolved secret value
  redacted. Output from the system under test is untrusted text: treat the
  answer as a suggestion. Each step is explained once; the answer is cached.
- **Hands off to your own agent.** The failure card's Copy context button
  and `lest context` give everything needed to pick up a failure: the flow and report paths, the
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
lest config set ai.model sonnet         # optional, passed to agent CLIs
lest config set ai.agent 'my-agent {prompt}'   # what Open in agent runs
lest config set ai.dailyLimit 50
lest config show
```

| Provider | Uses |
|---|---|
| `none` | No AI (default). |
| `auto` | The first supported agent CLI on PATH, else none. |
| a supported agent CLI | That CLI, headless, in an empty temporary directory, for one answer. CLIs that can turn tools off run with no tools, no MCP servers and no user customizations; a CLI that cannot runs in its read-only sandbox without the user's config or rules, so it can still read files to answer. |
| `command` | Any program that reads the prompt on stdin and prints the answer. Labeled by the program's name. |

The model defaults to the CLI's own small model. In Settings, Model lists
the aliases the resolved CLI accepts (for `claude`: Haiku, the default,
Sonnet, Opus and Fable), and Other takes any name the CLI understands. A
custom command receives no model, so the row is hidden for it. Switching
provider resets the model to the new CLI's default.

Every call, writing the prompt included, is stopped after 90 seconds, and
the provider's whole process group is killed. Prompts are cut at 48 KB. The
provider never sees the environment variables the env secret backends read;
its own configuration and sign-in variables are left alone.

Settings live in `config.yml` in Lest's config directory (`lest config show`
prints the path). Calls are counted per UTC day in the data directory; past
`dailyLimit`, explanations fall back to the headline with a note. Answers are
cached by the provider, command, model and input, so reopening a failure
costs nothing.

Open in agent passes the task to your agent through the `LEST_AGENT_PROMPT`
environment variable, so the agent command's quoting cannot turn the task's
text into shell code.

## Trust

The UI's token is equivalent to running commands as you: anyone holding it
can run the project's flows and, through Settings, set the command the AI
provider runs. Lest's server listens on loopback only, requires the token on
every API call, and accepts changes only from its own page. Do not share the
address `lest ui` prints.

## Without AI

Everything else is identical: the same failure card with the deterministic
headline, the same Copy context and rerun actions, the same CLI. Only the
menu items that need an agent are absent.
