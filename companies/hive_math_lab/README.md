# Hive Math Lab

> **Migration note.** The trace-grammar hive this bundle was written against
> (`[group_chat.hive]`: quorum, turn budget, blind round, per-seat moves,
> asides) has been replaced by completion-driven rooms — concurrent rounds,
> speech as a tool call, Jev routing, referral — configured by
> `[group_chat.routing]` ([`docs/spec/runtime/hive.md`](../../docs/spec/runtime/hive.md)).
> A `[group_chat.hive]` block is refused at load with a migration hint; the
> desks below keep their seats and their referral settings, and the grammar
> paragraphs describe the design the bundle was tuned under, not what runs.

The [Agentic Math Lab](../math_lab/README.md), re-seated on **one
desk** instead of four, so a stated problem is answered by a tinyhivemind
deliberation episode rather than by an orchestrator handing work from lead to
lead. The desk keeps the parent lab's three working roles and adds three more
instruments a hand-off chain had no seat for: a literal reading of the
statement, a brute force with no clever step to be wrong about, and a memory
of what this lab already knows. See `docs/spec/runtime/hive.md` for the
mechanics and `scripts/hive-euler.py` for the headless Project Euler driver.

## The solvers desk

Six members, six instruments. Three run on the strongest tier bought
(`agentic-v1`), reasoning-heavy work runs on `reasoning-v1`, and the three
that neither derive nor implement run on the cheap `chat-v1` tier — a diverse
roster, not a uniform one running six copies of the same model.

| Member | Tier → model | May `!propose`? | Job |
| --- | --- | --- | --- |
| `theorist` | reasoning → `reasoning-v1` | never | Reduces the problem, costs the naive method, pins the small-case table. |
| `programmer` | reasoning → `reasoning-v1` | **only member who may** | Writes and runs the program; the number on the floor is what it printed. |
| `verifier` | frontend → `agentic-v1` | never | An independent implementation written from the statement, not the program. |
| `skeptic` | none → `chat-v1` | never | Reads the statement literally; hunts inclusive/exclusive, base, and ordering misreadings. |
| `brute_forcer` | none → `chat-v1` | never | The naive method at a reduced bound, always with its command and output. |
| `archivist` | none → `chat-v1` | never | The desk's memory: recalls prior problems and methods, records what carried. |

A proposal needs a **quorum of three** grounded, differently-equipped
supporters to carry — not two identical `!propose`s from members who each
solved it alone, which is what a live run without this roster showed.

Run it locally against the ladder router and a CortexDB memory instance:

```bash
scripts/cortexdb-up.sh                     # prints the OPENCOMPANY_MEMORY_* exports
OPENCOMPANY_INFERENCE_KEY=$LADDER_API_KEY OPENCOMPANY_AUTH_MODE=none \
  cargo run --features openhuman --bin opencompany -- serve --company companies/hive_math_lab
python3 scripts/hive-euler.py --problems 1,5,12,31,60,100
```
## Tool servers

An answer here ships with the program that produced it, so the library documentation has to match the version that ran.

Declared in [`mcp.json`](mcp.json) and merged with anything the install
ships and anything an operator adds from the console. A server marked
*needs a token* is declared but off: write its credential from
Settings → Connections, then enable it there.

| Server | What it is for | Ships |
| --- | --- | --- |
| `deepwiki` | Documentation and Q&A for any public GitHub repository. Public and no-auth. | on |
| `context7` | Version-accurate API and library documentation, so answers match the release in use. | on |
| `huggingface` | Models, datasets and papers on the Hugging Face Hub. Public and no-auth. | on |
