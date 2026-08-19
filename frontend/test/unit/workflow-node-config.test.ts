import { describe, expect, it } from "vitest";

import {
  RESERVED_CONFIG_KEYS,
  configDraftFrom,
  configDraftProblem,
  configFieldProblem,
  configFieldSpecs,
  configFromDraft,
  hasConfigForm,
  nodeKindConfigProblem,
} from "@/lib/workflow-node-config";

/**
 * The five withheld node kinds' config forms (issue #541). The engine
 * (`vendor/openhuman/vendor/tinyflows`) reads exact keys off a node's `config`;
 * these helpers turn form strings into those keys and back. Getting a key
 * wrong ships a node that errors at run time, so the contract is pinned here:
 *
 * - each kind's valid draft serializes to EXACTLY the engine keys, empties
 *   omitted;
 * - a saved node's config round-trips through hydrate → serialize;
 * - an UNKNOWN config key (an orchestrator-authored `connection_ref`, a
 *   sub_workflow's `execution`) is PRESERVED — the anti-data-loss guard;
 * - malformed JSON and missing required fields are caught before submit;
 * - a sub_workflow can't call itself;
 * - the output NEVER carries a host-reserved key.
 */

describe("hasConfigForm", () => {
  it("covers the five withheld kinds plus condition (#661 M1) and transform (#661 L3)", () => {
    for (const kind of [
      "tool_call",
      "http_request",
      "switch",
      "output_parser",
      "sub_workflow",
      // `condition` is a core palette kind, but the host now REQUIRES
      // `config.field` at author time (#661 M1), so it carries a form too.
      "condition",
      // `transform` is also a core palette kind; its `config.set` map had no
      // control, so an authored transform lowered to a silent identity node
      // (#661 L3). It carries a form now.
      "transform",
    ]) {
      expect(hasConfigForm(kind), kind).toBe(true);
    }
    for (const kind of ["trigger", "agent", "merge", "output"]) {
      expect(hasConfigForm(kind), kind).toBe(false);
      expect(configFieldSpecs(kind)).toEqual([]);
    }
  });

  it("condition exposes a single required `field` (#661 M1)", () => {
    const specs = configFieldSpecs("condition");
    expect(specs).toHaveLength(1);
    expect(specs[0].key).toBe("field");
    expect(specs[0].required).toBe(true);
  });

  it("transform exposes a single optional object `set` field (#661 L3)", () => {
    const specs = configFieldSpecs("transform");
    expect(specs).toHaveLength(1);
    expect(specs[0].key).toBe("set");
    // OPTIONAL — an absent/empty `set` is a valid engine passthrough.
    expect(specs[0].required).toBeFalsy();
    // The engine reads `set` only via `as_object`, so the shape gate is `object`.
    expect(specs[0].jsonShape).toBe("object");
  });
});

/**
 * `configFromDraft` answers a Result rather than throwing (issue #1006), so
 * these cases — every one of which is about the config it emits — unwrap it
 * here. The failure path is pinned separately below.
 */
function serialized(
  ...args: Parameters<typeof configFromDraft>
): Record<string, unknown> | undefined {
  const out = configFromDraft(...args);
  if (!out.ok) throw new Error(`expected a serialized config, got: ${out.error}`);
  return out.config;
}

describe("configFromDraft — valid drafts emit exactly the engine keys", () => {
  it("tool_call: slug + parsed args, empties omitted", () => {
    expect(
      serialized("tool_call", { slug: "web_search", args: '{ "query": "hi" }' }),
    ).toEqual({ slug: "web_search", args: { query: "hi" } });

    // No args → the key is omitted entirely, not sent as "".
    expect(serialized("tool_call", { slug: "web_search", args: "" })).toEqual({
      slug: "web_search",
    });
  });

  it("http_request: method + url + parsed headers/body", () => {
    expect(
      serialized("http_request", {
        method: "POST",
        url: "https://api.example.com",
        headers: '{ "Authorization": "Bearer x" }',
        body: '{ "hello": "world" }',
      }),
    ).toEqual({
      method: "POST",
      url: "https://api.example.com",
      headers: { Authorization: "Bearer x" },
      body: { hello: "world" },
    });

    // A JSON string body is a valid body — it stays a string.
    expect(
      serialized("http_request", { method: "GET", url: "u", body: '"raw text"' }),
    ).toEqual({ method: "GET", url: "u", body: "raw text" });
  });

  it("switch: field or expression, whichever is filled", () => {
    expect(serialized("switch", { field: "status", expression: "" })).toEqual({
      field: "status",
    });
    expect(serialized("switch", { field: "", expression: "=item.score > 0.5" })).toEqual({
      expression: "=item.score > 0.5",
    });
  });

  it("output_parser: parsed schema + boolean auto_fix, default omitted", () => {
    expect(
      serialized("output_parser", {
        schema: '{ "type": "object" }',
        auto_fix: "false",
      }),
    ).toEqual({ schema: { type: "object" }, auto_fix: false });

    // Unset auto_fix (the "" default) is omitted so the engine's own default
    // (true) applies; an empty schema is omitted (identity parse).
    expect(serialized("output_parser", { schema: "", auto_fix: "" })).toBeUndefined();
    expect(serialized("output_parser", { schema: "", auto_fix: "true" })).toEqual({
      auto_fix: true,
    });
  });

  it("sub_workflow: workflow_id", () => {
    expect(serialized("sub_workflow", { workflow_id: "child-1" })).toEqual({
      workflow_id: "child-1",
    });
  });

  it("transform: set map, empty draft omits config entirely (#661 L3)", () => {
    expect(serialized("transform", { set: '{ "greeting": "=item.name" }' })).toEqual({
      set: { greeting: "=item.name" },
    });
    // An empty set is a passthrough → the whole config is omitted, never `{}`.
    expect(serialized("transform", { set: "" })).toBeUndefined();
    // An explicit empty object is authored through verbatim (still a passthrough).
    expect(serialized("transform", { set: "{}" })).toEqual({ set: {} });
  });

  it("tool_call: connection_ref rides alongside slug/args, omitted when blank (#661 M6)", () => {
    expect(
      serialized("tool_call", {
        slug: "slack.post",
        args: '{ "text": "hi" }',
        connection_ref: "composio:slack:acct_1",
      }),
    ).toEqual({ slug: "slack.post", args: { text: "hi" }, connection_ref: "composio:slack:acct_1" });

    // Whitespace-only connection_ref is trimmed to empty and omitted.
    expect(
      serialized("tool_call", { slug: "slack.post", args: "", connection_ref: "  " }),
    ).toEqual({ slug: "slack.post" });
  });

  it("http_request: connection_ref rides alongside method/url, omitted when blank (#661 M6)", () => {
    expect(
      serialized("http_request", {
        method: "POST",
        url: "https://api.test/x",
        connection_ref: "http:acct_2",
      }),
    ).toEqual({ method: "POST", url: "https://api.test/x", connection_ref: "http:acct_2" });

    expect(
      serialized("http_request", { method: "GET", url: "u", connection_ref: "" }),
    ).toEqual({ method: "GET", url: "u" });
  });

  it("an all-empty draft serializes to undefined (config omitted from the body)", () => {
    expect(serialized("switch", { field: "", expression: "" })).toBeUndefined();
    expect(serialized("http_request", { method: "", url: "" })).toBeUndefined();
    expect(serialized("transform", { set: "" })).toBeUndefined();
  });
});

describe("hydrate → serialize round-trips", () => {
  const cases: Array<{ kind: string; config: Record<string, unknown> }> = [
    { kind: "tool_call", config: { slug: "gmail.send", args: { to: "=item.email" } } },
    {
      kind: "http_request",
      config: {
        method: "POST",
        url: "=item.url",
        headers: { "X-Api-Key": "abc" },
        body: { q: "hi" },
      },
    },
    { kind: "switch", config: { field: "kind" } },
    { kind: "output_parser", config: { schema: { type: "object" }, auto_fix: false } },
    { kind: "sub_workflow", config: { workflow_id: "child-1" } },
    // `transform` flipping to a config form (#661 L3) means saved transform
    // configs now decompose → rebuild on every save; that MUST be lossless.
    { kind: "transform", config: { set: { greeting: "=item.name", tag: "fixed" } } },
    // connection_ref is a known key now (#661 M6), so it round-trips through the
    // draft — not the extra bag — on both integration kinds.
    {
      kind: "tool_call",
      config: { slug: "gmail.send", args: { to: "=item.email" }, connection_ref: "acct_9" },
    },
    {
      kind: "http_request",
      config: { method: "GET", url: "u", connection_ref: "http:acct_2" },
    },
  ];

  for (const { kind, config } of cases) {
    it(`${kind} survives a hydrate/serialize cycle`, () => {
      const { draft, extra } = configDraftFrom(kind, config);
      expect(serialized(kind, draft, extra)).toEqual(config);
    });
  }
});

describe("connection_ref is authorable, not an extra-bag rider (#661 M6)", () => {
  it("hydrates a tool_call connection_ref into the draft, leaving extra empty", () => {
    const config = { slug: "gmail.send", connection_ref: "acct_42", args: { to: "x" } };
    const { draft, extra } = configDraftFrom("tool_call", config);
    // Now a known key: it lands in the draft field, NOT the extra bag.
    expect(draft.connection_ref).toBe("acct_42");
    expect(extra).toEqual({});
    expect(draft.slug).toBe("gmail.send");
    // …and serializes back as a top-level config key.
    expect(serialized("tool_call", draft, extra)).toEqual(config);
  });

  it("blanking connection_ref on an edit deletes the key", () => {
    const config = { slug: "gmail.send", connection_ref: "acct_42" };
    const { draft, extra } = configDraftFrom("tool_call", config);
    draft.connection_ref = "";
    expect(serialized("tool_call", draft, extra)).toEqual({ slug: "gmail.send" });
  });

  it("hydrates an http_request connection_ref into the draft, leaving extra empty", () => {
    const config = { method: "GET", url: "u", connection_ref: "http:acct_2" };
    const { draft, extra } = configDraftFrom("http_request", config);
    expect(draft.connection_ref).toBe("http:acct_2");
    expect(extra).toEqual({});
    expect(serialized("http_request", draft, extra)).toEqual(config);
  });
});

describe("unknown-key preservation — the data-loss guard", () => {
  it("keeps an unknown sibling key on a transform across an edit (#661 L3)", () => {
    // transform gaining a config form must not start dropping unknown siblings.
    const config = { set: { g: "=item.name" }, mystery: "keep-me" };
    const { draft, extra } = configDraftFrom("transform", config);
    expect(draft.set).toBe(JSON.stringify({ g: "=item.name" }, null, 2));
    expect(extra).toEqual({ mystery: "keep-me" });
    expect(serialized("transform", draft, extra)).toEqual(config);
  });

  it("keeps a sub_workflow's execution/concurrency/inputs", () => {
    const config = {
      workflow_id: "child-1",
      execution: "per_item",
      concurrency: 3,
      inputs: { repo: "=inputs.repo" },
    };
    const { draft, extra } = configDraftFrom("sub_workflow", config);
    expect(extra).toEqual({
      execution: "per_item",
      concurrency: 3,
      inputs: { repo: "=inputs.repo" },
    });
    expect(serialized("sub_workflow", draft, extra)).toEqual(config);
  });
});

describe("output never contains a host-reserved key", () => {
  it("drops a reserved key smuggled into a saved config, on both hydrate and serialize", () => {
    const config = {
      slug: "gmail.send",
      on_error: "route",
      retry: { maxAttempts: 3 },
      requires_approval: true,
      schedule: "0 9 * * *",
      destination: { kind: "owner" },
      agent_ref: "someone",
    };
    const { draft, extra } = configDraftFrom("tool_call", config);
    for (const key of RESERVED_CONFIG_KEYS) {
      expect(extra, key).not.toHaveProperty(key);
    }
    const out = serialized("tool_call", draft, extra) ?? {};
    for (const key of RESERVED_CONFIG_KEYS) {
      expect(out, key).not.toHaveProperty(key);
    }
    expect(out).toEqual({ slug: "gmail.send" });
  });

  it("never re-emits a reserved key even if forced through the extra bag", () => {
    const out = serialized("http_request", { method: "GET", url: "u" }, {
      on_error: "route",
      requires_approval: true,
    });
    expect(out).toEqual({ method: "GET", url: "u" });
  });
});

describe("configFromDraft answers a Result, never a throw (#1006)", () => {
  it("reports malformed JSON instead of throwing out of the caller's `try`", () => {
    // Only reachable when `configDraftProblem` and this disagree — but the
    // caller assembles the graph inside the submit path, and an exception from
    // here skipped the `finally` that clears `submitting`, locking the dialog
    // shut over the operator's unsaved work.
    const out = configFromDraft("tool_call", { slug: "web_search", args: "{ oops" });
    expect(out).toEqual({ ok: false, error: "Arguments must be valid JSON." });
  });

  it("wraps a valid serialization, empty configs included", () => {
    expect(configFromDraft("switch", { field: "status", expression: "" })).toEqual({
      ok: true,
      config: { field: "status" },
    });
    expect(configFromDraft("switch", { field: "", expression: "" })).toEqual({
      ok: true,
      config: undefined,
    });
  });
});

describe("configFieldProblem — blur-time JSON check", () => {
  const argsSpec = configFieldSpecs("tool_call").find((s) => s.key === "args")!;
  const slugSpec = configFieldSpecs("tool_call").find((s) => s.key === "slug")!;

  it("flags malformed JSON on a json field", () => {
    expect(configFieldProblem(argsSpec, "{ not json")).toMatch(/must be valid JSON/);
  });

  it("accepts valid JSON, and never nags an empty field", () => {
    expect(configFieldProblem(argsSpec, '{ "a": 1 }')).toBeNull();
    expect(configFieldProblem(argsSpec, "")).toBeNull();
    // A non-JSON control has no blur rule of its own.
    expect(configFieldProblem(slugSpec, "anything")).toBeNull();
  });

  it("rejects a valid-JSON value whose shape the engine key can't take", () => {
    const headersSpec = configFieldSpecs("http_request").find((s) => s.key === "headers")!;
    const bodySpec = configFieldSpecs("http_request").find((s) => s.key === "body")!;

    // `args`/`headers` must be objects — arrays and scalars are valid JSON but wrong.
    for (const bad of ["[]", "42", "true", '"a string"', "null"]) {
      expect(configFieldProblem(argsSpec, bad)).toMatch(/must be a JSON object/);
      expect(configFieldProblem(headersSpec, bad)).toMatch(/must be a JSON object/);
    }

    // A body may be an object OR a bare string, but not an array/number/boolean.
    expect(configFieldProblem(bodySpec, '"raw text"')).toBeNull();
    expect(configFieldProblem(bodySpec, '{ "hello": "world" }')).toBeNull();
    for (const bad of ["[]", "42", "true", "null"]) {
      expect(configFieldProblem(bodySpec, bad)).toMatch(
        /must be a JSON object or a JSON string/,
      );
    }
  });

  it("rejects a transform set that is valid JSON but not an object (#661 L3)", () => {
    const setSpec = configFieldSpecs("transform").find((s) => s.key === "set")!;
    // The engine reads `set` via `as_object`, so arrays/scalars are silently
    // ignored at run time — the shape gate rejects them at author time instead.
    for (const bad of ["[]", "42", "true", '"a string"', "null"]) {
      expect(configFieldProblem(setSpec, bad)).toMatch(/must be a JSON object/);
    }
    expect(configFieldProblem(setSpec, "{ not json")).toMatch(/must be valid JSON/);
    expect(configFieldProblem(setSpec, '{ "greeting": "=item.name" }')).toBeNull();
    // Empty is never nagged on blur.
    expect(configFieldProblem(setSpec, "")).toBeNull();
  });
});

describe("configDraftProblem — submit-time gate", () => {
  it("requires a tool_call slug", () => {
    expect(configDraftProblem("tool_call", "n1", { slug: "", args: "" })).toMatch(
      /needs a tool slug/,
    );
    expect(configDraftProblem("tool_call", "n1", { slug: "web_search", args: "" })).toBeNull();
  });

  it("requires an http_request url", () => {
    expect(configDraftProblem("http_request", "n1", { method: "GET", url: "" })).toMatch(
      /needs a URL/,
    );
  });

  it("rejects malformed JSON at submit even if blur was skipped", () => {
    expect(
      configDraftProblem("tool_call", "n1", { slug: "x", args: "{ oops" }),
    ).toMatch(/must be valid JSON/);
  });

  it("requires a switch to name a field or an expression", () => {
    expect(configDraftProblem("switch", "n1", { field: "", expression: "" })).toMatch(
      /field or an expression/,
    );
    expect(configDraftProblem("switch", "n1", { field: "status", expression: "" })).toBeNull();
    expect(
      configDraftProblem("switch", "n1", { field: "", expression: "=item.x" }),
    ).toBeNull();
  });

  it("rejects a switch that supplies both a field and an expression", () => {
    expect(
      configDraftProblem("switch", "n1", { field: "status", expression: "=item.x" }),
    ).toMatch(/field or an expression, not both/);
  });

  it("rejects an http_request json field whose shape is wrong at submit", () => {
    expect(
      configDraftProblem("http_request", "n1", { method: "GET", url: "u", headers: "[]" }),
    ).toMatch(/must be a JSON object/);
    expect(
      configDraftProblem("http_request", "n1", { method: "GET", url: "u", body: "42" }),
    ).toMatch(/must be a JSON object or a JSON string/);
  });

  it("requires an output_parser schema to parse when present", () => {
    expect(
      configDraftProblem("output_parser", "n1", { schema: "{ bad", auto_fix: "" }),
    ).toMatch(/must be valid JSON/);
    // Absent schema is fine — the engine treats it as identity.
    expect(configDraftProblem("output_parser", "n1", { schema: "", auto_fix: "" })).toBeNull();
  });

  it("rejects a sub_workflow pointed at its own id", () => {
    expect(
      configDraftProblem("sub_workflow", "flow-1", { workflow_id: "flow-1" }),
    ).toMatch(/can't call itself/);
    expect(
      configDraftProblem("sub_workflow", "flow-1", { workflow_id: "flow-2" }),
    ).toBeNull();
    // Missing id is a required-field problem, not the self-reference one.
    expect(configDraftProblem("sub_workflow", "flow-1", { workflow_id: "" })).toMatch(
      /needs the id/,
    );
  });

  it("rejects a transform set whose shape is wrong at submit (#661 L3)", () => {
    expect(configDraftProblem("transform", "n1", { set: "[]" })).toMatch(/must be a JSON object/);
    expect(configDraftProblem("transform", "n1", { set: "{ bad" })).toMatch(/must be valid JSON/);
    // An empty set is a valid passthrough — never blocked.
    expect(configDraftProblem("transform", "n1", { set: "" })).toBeNull();
    expect(
      configDraftProblem("transform", "n1", { set: '{ "greeting": "=item.name" }' }),
    ).toBeNull();
  });

  it("is a no-op for a kind without a config form", () => {
    expect(configDraftProblem("agent", "n1", {})).toBeNull();
  });
});

/**
 * Issue #783: the console mirror of the host's write-time kind↔config rules,
 * used by the copilot proposal path to refuse a "wrong kind when applied" node
 * before the operator is shown a diff for it. It must match the host
 * (`required_config_problems` + the `agent` arm of `validate_draft_against_record`)
 * — no stricter, or it would turn away a proposal the host accepts.
 */
describe("nodeKindConfigProblem", () => {
  it("refuses a tool_call with no config.slug and accepts one with it", () => {
    expect(nodeKindConfigProblem({ kind: "tool_call" })).toMatch(/config\.slug/);
    expect(nodeKindConfigProblem({ kind: "tool_call", config: { slug: "web_search" } })).toBeNull();
  });

  it("refuses an agent naming no teammate, and accepts one that does", () => {
    expect(nodeKindConfigProblem({ kind: "agent" })).toMatch(/teammate/);
    // The teammate is the top-level `agent` field, never inside config.
    expect(nodeKindConfigProblem({ kind: "agent", config: { agent: "analyst" } })).toMatch(
      /teammate/,
    );
    expect(nodeKindConfigProblem({ kind: "agent", agent: "analyst" })).toBeNull();
  });

  it("requires both method and url on an http_request", () => {
    expect(nodeKindConfigProblem({ kind: "http_request", config: { url: "https://x" } })).toMatch(
      /config\.method/,
    );
    expect(nodeKindConfigProblem({ kind: "http_request", config: { method: "GET" } })).toMatch(
      /config\.url/,
    );
    expect(
      nodeKindConfigProblem({ kind: "http_request", config: { method: "GET", url: "https://x" } }),
    ).toBeNull();
  });

  it("takes a switch discriminant as field OR expression, matching the host", () => {
    expect(nodeKindConfigProblem({ kind: "switch" })).toMatch(/discriminant/);
    expect(nodeKindConfigProblem({ kind: "switch", config: { field: "status" } })).toBeNull();
    expect(nodeKindConfigProblem({ kind: "switch", config: { expression: "=x>0" } })).toBeNull();
    // The host accepts both set; the stricter form validator does not, but this
    // mirror must not — a proposal the host takes is never refused here.
    expect(
      nodeKindConfigProblem({ kind: "switch", config: { field: "status", expression: "=x>0" } }),
    ).toBeNull();
  });

  it("requires a condition field and a sub_workflow workflow_id", () => {
    expect(nodeKindConfigProblem({ kind: "condition" })).toMatch(/config\.field/);
    expect(nodeKindConfigProblem({ kind: "condition", config: { field: "=item.ok" } })).toBeNull();
    expect(nodeKindConfigProblem({ kind: "sub_workflow" })).toMatch(/config\.workflow_id/);
    expect(
      nodeKindConfigProblem({ kind: "sub_workflow", config: { workflow_id: "other" } }),
    ).toBeNull();
  });

  it("imposes no config rule on kinds the host requires none of", () => {
    // These are valid host kinds with no required config — a proposal must not
    // be refused for omitting config it never needed.
    for (const kind of ["trigger", "output", "merge", "split_out", "transform", "output_parser"]) {
      expect(nodeKindConfigProblem({ kind })).toBeNull();
    }
  });
});
