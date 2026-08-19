import { describe, expect, it } from "vitest";

import { OpenCompanyClient } from "@/api/client";
import type {
  StreamHandlers,
  Transport,
  TransportRequest,
  TransportResponse,
} from "@/api/transport";
import { ApiError, workflowProblemLocator } from "@/api/types";

/**
 * The `problems` breakdown survives the wire (issue #836).
 *
 * The host has answered a refused workflow graph with
 * `{error, code: "workflow_invalid", problems: [...]}` since #1016, each entry
 * naming the node and config field at fault. The console parsed `error` and
 * `code` and dropped `problems` on the floor, so an operator was told *that*
 * the graph was refused and never *which node* — the copilot's Apply button
 * being the surface where that hurts, because the operator did not author the
 * change and has nothing to reread.
 *
 * These pin the parse, and — since review found a bug in the half that had none
 * — the locator each problem renders. There is still no component-test harness
 * in this project (no `@testing-library/react` anywhere under `test/`), so the
 * markup that *places* that locator is guarded by the compiler and by reading.
 * Said out loud rather than implied: the transport and the locator are covered
 * here, the JSX around them is not.
 */

/** Answers with whatever the test staged, so no `fetch` is involved. */
class StubTransport implements Transport {
  constructor(private readonly staged: Partial<TransportResponse>) {}

  async request(req: TransportRequest): Promise<TransportResponse> {
    return {
      status: this.staged.status ?? 200,
      statusText: this.staged.statusText ?? "",
      url: this.staged.url ?? req.url,
      text: this.staged.text ?? "",
      header: this.staged.header ?? (() => null),
    };
  }

  subscribe(_url: string, _handlers: StreamHandlers): () => void {
    return () => {};
  }
}

/** The `ApiError` a refused call threw, typed so assertions need no casts. */
async function refusal(body: unknown, status = 400): Promise<ApiError> {
  const client = new OpenCompanyClient(
    { baseUrl: "", company: null, operatorToken: null, sessionHeader: null },
    new StubTransport({ status, text: JSON.stringify(body) }),
  );
  try {
    await client.get("/api/v1/company/workflows/x");
  } catch (error) {
    return error as ApiError;
  }
  throw new Error("expected the call to reject");
}

const invalid = (problems: unknown) => ({
  error: "greet has a bad url.",
  code: "workflow_invalid",
  problems,
});

describe("a refused workflow graph carries its per-node problems", () => {
  it("keeps the node and field the host named", async () => {
    const err = await refusal(
      invalid([
        { node_id: "greet", field: "config.url", message: "greet has a bad url." },
        { node_id: "post", field: "config.set", message: "post sets an unknown key." },
      ]),
    );

    expect(err.code).toBe("workflow_invalid");
    expect(err.problems).toEqual([
      { node_id: "greet", field: "config.url", message: "greet has a bad url." },
      { node_id: "post", field: "config.set", message: "post sets an unknown key." },
    ]);
    // The flat sentence stays the fallback; the breakdown is additive.
    expect(err.message).toBe("greet has a bad url.");
  });

  it("keeps a graph-level problem that owns no node or field", async () => {
    const err = await refusal(invalid([{ message: "the graph has an inescapable cycle." }]));
    expect(err.problems).toEqual([{ message: "the graph has an inescapable cycle." }]);
  });

  it("drops an entry with no readable message rather than rendering junk", async () => {
    const err = await refusal(
      invalid([
        { node_id: "greet", message: "greet has a bad url." },
        { node_id: "post", message: "   " },
        { node_id: "other" },
        "not an object",
        null,
      ]),
    );
    expect(err.problems).toEqual([{ node_id: "greet", message: "greet has a bad url." }]);
  });

  it("drops a locator that is not a string, keeping the message", async () => {
    const err = await refusal(invalid([{ node_id: 7, field: {}, message: "something is wrong." }]));
    expect(err.problems).toEqual([{ message: "something is wrong." }]);
  });

  /**
   * "Every entry was junk" must not read as "a breakdown with nothing in it" —
   * a renderer branching on `problems` would otherwise open an empty list.
   */
  it("reports no breakdown at all when nothing survives the filter", async () => {
    const err = await refusal(invalid([{ node_id: "greet" }, 42]));
    expect(err.problems).toBeUndefined();
  });

  it("reports no breakdown when the key is absent or not an array", async () => {
    const absent = await refusal({ error: "nope.", code: "company_not_found" }, 404);
    expect(absent.problems).toBeUndefined();

    const wrongShape = await refusal(invalid({ greet: "bad url" }));
    expect(wrongShape.problems).toBeUndefined();
    // The envelope itself still parsed, so the operator keeps the sentence.
    expect(wrongShape.code).toBe("workflow_invalid");
  });
});

/**
 * Where a problem says it happened — the half that had no test and was wrong.
 *
 * Review caught the field-only shape rendering as a bare message: the locator
 * was keyed on `node_id`, so a problem carrying a field and no node lost the one
 * piece of information this feature exists to surface. It could not have been
 * caught by a test, because the logic lived inside JSX in a project with no
 * component-test harness. Pulling it into a function is the fix for that, not
 * only for the bug.
 */
describe("a problem's locator", () => {
  it("joins the node and the field when it has both", () => {
    expect(workflowProblemLocator({ node_id: "greet", field: "config.url", message: "m" })).toBe(
      "greet · config.url",
    );
  });

  it("names the node alone when there is no field", () => {
    expect(workflowProblemLocator({ node_id: "greet", message: "m" })).toBe("greet");
  });

  /** The regression: reachable whenever the host stores a blank node id. */
  it("names the field alone when there is no node", () => {
    expect(workflowProblemLocator({ field: "config.url", message: "m" })).toBe("config.url");
  });

  it("has nothing to say about a graph-level problem", () => {
    expect(workflowProblemLocator({ message: "the graph has an inescapable cycle." })).toBeUndefined();
  });

  /** A blank string is not a locator; it would render a dangling separator. */
  it("ignores blank parts rather than rendering an empty locator", () => {
    expect(workflowProblemLocator({ node_id: "  ", field: "", message: "m" })).toBeUndefined();
    expect(workflowProblemLocator({ node_id: "  ", field: "config.url", message: "m" })).toBe(
      "config.url",
    );
  });
});
