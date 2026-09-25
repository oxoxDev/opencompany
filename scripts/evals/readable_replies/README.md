# Readable replies eval

An opt-in check on how agent replies read to a person: how long they are, and
how often internal ids, tool names or protocol text reach them. Nothing in CI
runs it. It needs a real model, so it costs tokens and its numbers move from
run to run; read it as a trend, not a gate.

## What it does

`run.sh` boots `opencompany serve` on loopback over the fixture company in
`company/`, pointed at a **staging** model, then runs `run.mjs` against it.
`run.mjs` signs in with the echoed dev code, sends 12 fixed prompts (6 direct
messages, 6 desk messages) `--samples` times each (default 3), and reads every
agent reply back from `GET /api/v1/company/chat/history`.

Four desk prompts send a follow-up on the same desk once the first answer has
landed. The follow-up is what reaches a seat on a seeded turn (turn 2 and
later), which is where a reply used to lose the writing-style rules.

## What it reports

For each reply it scores two texts:

| Text | Source | Meaning |
| --- | --- | --- |
| raw | `cueText` when present, else `text` | what the model wrote; the journal keeps this |
| shown | `text` | what a person sees after the display projection |

and prints:

- replies counted
- words per reply, p50 and p90
- non-empty lines per reply, p50 and p90
- leak hits, raw vs shown, by kind:

| Kind | Matches |
| --- | --- |
| `roster_or_desk_id` | any id from `company/company.toml` or `company/agents/*.toml`, bare, backticked or after `@` |
| `tool_name` | a registered tool name (`delegate_to_desk`, `query_company`, `desk_ask`, ...) |
| `card_or_task_id` | `task 12`, `card #3`, or a UUID |
| `run_id` | `run_id`, or `run-` / `run_` followed by an id |
| `sequence_number` | `sequence 42`, `seq 42` |
| `conversation_prefix` | the pooled turn's `[conversation: ...]` preamble |
| `bare_json` | a prose line that opens with `{` or `[` and carries `"key":` |

Fenced code blocks are not counted. The fixture's ids all contain `_` so an
ordinary word is never mistaken for one.

Exit code: `0` when the shown text has no leak hits, `1` when it has some, and
`9x` for a harness problem (missing credential, refused URL, host never came
up, sign-in failed). `--json` prints the whole report, every reply included.

## Running it

Build the binary with the harness in it:

```sh
cargo build --locked --features openhuman,mcp --bin opencompany
```

Then point it at a staging model. The script refuses any `tinyhumans.ai` URL
that is not a staging one, and `run.mjs` refuses any host that is not
loopback.

```sh
OPENCOMPANY_INFERENCE_URL=<staging OpenAI-compatible base URL> \
OPENCOMPANY_INFERENCE_KEY=<staging key> \
OPENCOMPANY_INFERENCE_MODEL=<model id> \
  scripts/evals/readable_replies/run.sh --samples 3
```

Against a host you already run (it must bind loopback, list
`harness-e2e@tinyhumans.ai` as an admin, and have no mail transport, or the dev
code is mailed rather than echoed):

```sh
node scripts/evals/readable_replies/run.mjs --base http://127.0.0.1:8080 \
  --company scripts/evals/readable_replies/company
```

Knobs: `--samples N`, `--seconds N` (how long to wait for one prompt's replies,
default 240), `--json`. `run.sh` also reads `READABLE_BIND`,
`READABLE_DATA_DIR` and `READABLE_BINARY`.

## Reading the result

- **Raw leak hits** are the prompts' job. They should fall as the reader brief
  and the name-first team brief take hold; a rise means a prompt change taught
  ids back.
- **Shown leak hits** are the projection's job. For roster and desk ids and the
  conversation preamble they should be zero; the other kinds are not rewritten
  at display, so a hit there is a prompt problem the projection cannot hide.
- **Length** has no threshold. p50 around a few short sentences is the aim;
  a p90 far above it usually means one kind of prompt still gets a dump.

## Never

- Run it against production. Staging only.
- Add it to CI. It needs a live model and its numbers are not stable.
