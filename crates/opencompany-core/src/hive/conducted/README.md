# `hive/conducted/`

Child modules of `hive/conducted.rs`, which stays the module root: it owns
`HiveDispatcher`, `Episode`, `run`/`resume` and the episode lifecycle, and each
file here is one coherent step of that lifecycle lifted out to keep the root
under the 750-line cap.

| File | What lives here |
| --- | --- |
| `closing.rs` | `HiveDispatcher::conclusion`, the step between the last seat settling and the closing row being journaled. With a routing oracle it asks `hive::conclude::decide` — one System One call carrying both questions — which either names the seat or answers `NotNeeded`, and a `NotNeeded` skips the turn entirely; without one it falls back to `hive::conclude::pick_concluder` over `route_desk` and always concludes. The chosen seat runs as a one-seat `Episode` with `concluding: true`, and its message's sequence is read back out of the journal for `EpisodeCompleted.summary_seq` — only from rows above the watermark taken before the round, so a turn that recorded nothing leaves `None` rather than naming an earlier deliberation row. Failure is warned and swallowed — the episode has settled, and losing a finished episode over an extra turn would be the worse trade. Why the step exists at all is documented on `hive::conclude`. Gated on `openhuman`. |
