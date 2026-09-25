# Hive episodes and rounds

How a desk of two or more answers as a room, and the journal rows that record
it. Split out of [`events.md`](events.md) to keep that file under the
repository's 500-line Markdown ceiling; the frame table and the permanence rule
are unchanged.

A desk of two or more answers as a room. The host opens one **episode** per
operator message (or thread root) on that desk's hive, routes it to seats with
a `RoutingPlan`, and runs the seats **concurrently** in **rounds** until one
calls `complete_episode` — or the round cap, a timeout, a failed turn or a
membership change ends it. Every step is journaled, and every journal row is
projected onto `/events` under the frame name below, all on the usual
`{type, seq, atMillis}` envelope with `chatId` = the desk:

| frame | fields |
| --- | --- |
| `episode_opened` | `episodeId, openedBySeq, parentId?, participants[], plan` |
| `round_started` | `episodeId, revision, agentIds[]` — these seats run together |
| `turn_started` (+) | `agentId?, episodeId?, roundRevision?` on the #983 bracket |
| `turn_settled` | `turnId, agentId?, episodeId?, roundRevision?, outcome: committed \| failed \| timed_out \| no_utterance` — now on success too |
| `round_committed` | `episodeId, revision, utterances[{agentId, sequence, kind, messageSeq?, to?}], actions[]` |
| `broadcast_routed` | `episodeId, revision, agentId, messageSeq, plan, probabilities?, router: jev \| fallback \| explicit` |
| `dm_delivered` | `episodeId, from, to[], messageSeq` |
| `episode_completed` | `episodeId, revision, completedBy?, rounds, reason, summarySeq?` |
| `referral` (+) | `episodeId?, toEpisodeId?` — the asking and the answering episode |
| `desk_routing_configured` | `deskId, reset` (replaces `desk_hive_configured`) |
| `tool_call` / `tool_result` / `thinking` (+) | `episodeId?, roundRevision?` (ephemeral) |

`plan` is `{kind:"one", primaryId}`, `{kind:"hive", primaryId, invitedIds[]}`,
`{kind:"clarify", question?}` or `{kind:"fallback", reason}`. A seat's
committed utterance is an ordinary `AgentReply` row carrying
`episode: {id, revision, kind: post | broadcast | dm | complete_episode, to?,
routedBy?: {plan, router}}` and, for a desk `dm`, `audience: [agent ids]` — so
`chat/history` alone rebuilds every round after a reload, and the frames add
only the present tense: which seats a round opened with, which is still
working, which said nothing. The console folds the two in
`frontend/src/lib/episodes.ts`; `scripts/measure-coordination.mjs` folds the
frames alone into the coordination numbers (peak concurrent turns, same-agent
overlaps — always zero, one agent runs one turn at a time across every desk it
sits on — rounds per episode, dms, broadcasts, cross-desk referrals).

`EpisodeOpened` and `EpisodeCompleted` are **permanent**; the round, broadcast
and dm rows are **prunable** — the reply rows they point at are the evidence,
and a completed episode is replayed from those.
