import { describe, expect, it } from "vitest";

import {
  composeCopilotMessage,
  copilotThreadId,
  questionOf,
  type CopilotContext,
} from "@/api/workflow-copilot";
import type { WorkflowGraph } from "@/api/workflows";

/**
 * Issues #303 and #416.
 *
 * The copilot's whole boundary is carried by two strings: the thread id, which
 * is what the host reads to decide a turn runs confined
 * (`src/company/copilot.rs`), and the composed message, which is now the only
 * material the answering agent has — it has no tools and no company memory, so
 * anything not in here is not answerable.
 *
 * That makes both worth pinning. A thread id that stopped matching the host's
 * prefix would silently return the copilot to the company orchestrator, with
 * every property looking unchanged from the console's side.
 */

const graph: WorkflowGraph = {
  id: "weekly_report",
  name: "Weekly report",
  description: "Assemble and send the Monday summary.",
  nodes: [
    {
      id: "collect",
      kind: "agent",
      name: "Collect",
      agent: "analyst",
      summary: "Pull last week's numbers",
    },
    { id: "send", kind: "report", name: "Send", destination: { kind: "email", target: "team" } },
  ],
  edges: [{ from: "collect", to: "send" }],
};

const context: CopilotContext = { graph, runs: [], runsKnown: true };

describe("copilotThreadId", () => {
  /** The exact string `company::copilot::workflow_of_thread` parses. */
  it("is the prefix the host confines on", () => {
    expect(copilotThreadId("weekly_report")).toBe("workflow-copilot:weekly_report");
  });
});

describe("composeCopilotMessage", () => {
  it("carries the workflow's own graph as the grounding", () => {
    const message = composeCopilotMessage(context, "why is it slow?");
    expect(message).toContain("weekly_report");
    expect(message).toContain("collect");
    expect(message).toContain("send");
    expect(message).toContain("why is it slow?");
  });

  /**
   * #416. The message states the confinement the host now enforces: no tools,
   * nothing outside this workflow, and what to do at the edge. The panel builds
   * its "what can it see?" disclosure from this same function, so a message that
   * stopped saying it would make the disclosure a claim about nothing.
   */
  it("states the boundary and what to do when a question exceeds it", () => {
    const message = composeCopilotMessage(context, "what is the team working on?");
    expect(message).toContain("no tools");
    expect(message).toContain("company chat");
    expect(message).toMatch(/cannot reach anything else in the company/i);
    expect(message).toMatch(/do not claim to have looked anything up/i);
  });

  /**
   * #415 replaced "you cannot edit" with the sharper pair: it cannot APPLY,
   * and it may PROPOSE. Both halves matter — a model told only the first goes
   * back to prose the operator has to retype, and one told only the second
   * will claim to have made the change.
   */
  it("may propose a change and may not apply one", () => {
    const message = composeCopilotMessage(context, "add a retry");
    expect(message).toMatch(/CANNOT apply a change yourself/);
    expect(message).toMatch(/PROPOSE/);
    expect(message).toContain("## Proposing a change");
    expect(message).toContain("```workflow-proposal");
    // The ops the console can actually read back.
    for (const op of ["addNode", "updateNode", "removeNode", "addEdge", "removeEdge"]) {
      expect(message).toContain(op);
    }
  });

  /**
   * A source-defined workflow is refused every write by the host, so proposing
   * one could only ever produce a diff whose Apply is refused. The protocol is
   * withheld entirely rather than offered and then blocked.
   */
  it("does not offer the protocol for a workflow the console cannot write", () => {
    const message = composeCopilotMessage(
      { ...context, graph: { ...graph, editable: false } },
      "add a retry",
    );
    expect(message).not.toContain("## Proposing a change");
    expect(message).toMatch(/must not propose an edit/);
  });

  /**
   * The transcript the operator reads back is their question, not the several
   * hundred lines of grounding sent with it — including when the question
   * itself contains the marker.
   */
  it("round-trips the operator's question back out of the composed message", () => {
    const question = "why did it fail?\n\n### The operator's question\nand then?";
    expect(questionOf(composeCopilotMessage(context, question))).toBe(question);
  });

  /**
   * Issue #783. The proposals were ungrounded: the message never told the model
   * which teammates or tools exist, so it guessed ones the host then rejected.
   * The composed message now inlines the real roster ids and the real tool
   * slugs, so a proposal can name what actually exists.
   */
  it("grounds the model in the real roster ids and tool slugs", () => {
    const grounded: CopilotContext = {
      ...context,
      roster: [
        { id: "analyst", role: "Analyst" },
        { id: "editor", role: "Editor" },
      ],
      toolSlugs: ["web_search", "send_email"],
      toolSlugsKnown: true,
    };
    const message = composeCopilotMessage(grounded, "add a research step");
    expect(message).toContain("analyst");
    expect(message).toContain("editor");
    expect(message).toContain("web_search");
    expect(message).toContain("send_email");
  });

  /**
   * The kind-specific config schema is inlined too, so the model knows the keys
   * live INSIDE `config` and which each kind needs — the fix for proposals that
   * put `slug`/`repo` as top-level node fields the host's allowlist refused.
   */
  it("teaches the config schema and that kind-specific keys nest under config", () => {
    const message = composeCopilotMessage(context, "call a tool");
    // The shape rule and a worked example.
    expect(message).toMatch(/inside a `?config`? object/i);
    expect(message).toContain("config.slug (required)");
    expect(message).toContain('"config": {"slug": "web_search"');
    // The enumerated kinds and the id-invention guard.
    for (const kind of ["tool_call", "agent", "http_request", "sub_workflow"]) {
      expect(message).toContain(kind);
    }
    expect(message).toMatch(/only node ids that exist/i);
  });

  /**
   * Honest absence, the same split `runsKnown` makes: a host that does not serve
   * the tool list must not be told "no tools" — that would suppress a legitimate
   * `tool_call`. It is told the tools could not be listed instead.
   */
  it("distinguishes an unlisted tool set from a genuinely empty one", () => {
    const unlisted = composeCopilotMessage(
      { ...context, toolSlugs: undefined, toolSlugsKnown: false },
      "call a tool",
    );
    expect(unlisted).toMatch(/could not be listed/i);

    const empty = composeCopilotMessage(
      { ...context, toolSlugs: [], toolSlugsKnown: true },
      "call a tool",
    );
    expect(empty).toMatch(/no tools are granted/i);
  });
});
