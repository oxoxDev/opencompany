// The five withheld node kinds' config forms (issue #541, P4). The pure half:
// the field table both the create dialog and its renderer read from, and the
// hydrate/serialize/validate helpers that turn a saved node's free-form
// `config` into form strings and back.
//
// The engine (`vendor/openhuman/vendor/tinyflows`) parses and runs all five
// kinds already; only the console lacked controls to author their config, so
// `tool_call`/`http_request`/`switch`/`output_parser`/`sub_workflow` were kept
// off the palette (`CREATABLE_NODE_KINDS`) rather than shipping a node that
// errors at run time. These forms emit EXACTLY the keys the tinyflows executors
// read — verified against `catalog.rs`, `nodes/integration/*`, and
// OpenCompany's `translate.rs` (which lays a node's `config` over the derived
// defaults verbatim):
//
//   tool_call     { slug, args, connection_ref }
//   http_request  { method, url, headers, body, connection_ref }
//   switch        { field | expression }        (cases are EDGE labels, not config)
//   output_parser { schema, auto_fix }
//   sub_workflow  { workflow_id }
//   transform     { set }
//
// `condition` is a core palette kind (not one of the withheld five), but the
// host now REQUIRES `config.field` at author time (#661 M1) — the engine
// truthiness-tests that expression, and without it a condition always routed
// `true`. So it carries a form too: `condition { field }` (the `yes`/`no`
// branches are EDGE labels, not config).
//
// `transform` is likewise a core palette kind, but its one engine key —
// `config.set`, a JSON object of field → literal/`=`-expression the engine lays
// over each item (`nodes/control_flow/transform.rs`) — had no control, so a
// console-authored transform lowered to a silent identity node. It carries a
// form now: `transform { set }`. `set` is OPTIONAL — an absent/empty `set` is a
// valid engine passthrough (what a plain `output` node lowers to), so it is
// never required. The engine reads `set` only via `as_object`, so a non-object
// `set` is silently ignored; the `object` shape gate rejects it at author time.
//
// `tool_call` and `http_request` also each read an optional `connection_ref`
// (`cfg.get("connection_ref").and_then(as_str)` in their integration nodes) —
// an opaque handle to a connected account / credential the host resolves, never
// a token or secret. It used to survive an edit only by riding the `extra` bag
// (so it was unauthorable); it is a first-class field on both kinds now.
//
// Keys the host REJECTS inside `config` — `on_error`, `retry`,
// `requires_approval`, `schedule`, `destination`, `agent_ref` — are first-class
// node fields and are never emitted here (see `src/company/workflow_file.rs`).

/** How a config field is rendered. `workflow-ref` is a picker fed by the
 * company's other workflows (with a free-text fallback). */
export type ConfigControl = "line" | "json" | "select" | "workflow-ref";

/** One authored config field: its engine key, how it renders, and its rules. */
export interface ConfigFieldSpec {
  /** The engine config key this field writes (e.g. `slug`, `url`). */
  key: string;
  label: string;
  control: ConfigControl;
  placeholder?: string;
  /** A one-line explanation shown under the control. */
  hint?: string;
  /** Blocks submit when empty (checked in {@link configDraftProblem}). */
  required?: boolean;
  /** The draft's starting value for this field (else `""`). */
  default?: string;
  /** `select` options. An option whose `value` is `""` means "unset / omit". */
  options?: readonly { value: string; label: string }[];
  /** A `select` whose chosen value serializes as a JSON boolean (`auto_fix`). */
  boolean?: boolean;
  /**
   * For a `json` field, the value shape the engine key accepts. Absent = any
   * valid JSON. `object` rejects arrays/scalars (e.g. `args`, `headers`);
   * `object-or-string` also allows a bare JSON string (e.g. an HTTP `body`).
   */
  jsonShape?: "object" | "object-or-string";
}

/**
 * The per-kind field table. A kind absent from here has no config form — its
 * `config` (if any) rides through an edit untouched, as before.
 */
export const NODE_CONFIG_FIELDS: Record<string, readonly ConfigFieldSpec[]> = {
  condition: [
    {
      key: "field",
      label: "Field",
      control: "line",
      required: true,
      placeholder: "=item.approved",
      hint: "The boolean expression the branch tests. The `yes` edge takes the true branch, `no` the false.",
    },
  ],
  tool_call: [
    {
      key: "slug",
      label: "Tool slug",
      control: "line",
      required: true,
      placeholder: "e.g. web_search",
      hint: "The tool's id. Without it the engine falls back to the node id and runs the wrong tool.",
    },
    {
      key: "args",
      label: "Arguments",
      control: "json",
      jsonShape: "object",
      placeholder: '{ "query": "=item.topic" }',
      hint: "A JSON object of arguments. `=`-expressions resolve per input item at run time.",
    },
    {
      key: "connection_ref",
      label: "Connection",
      control: "line",
      placeholder: "composio:slack:acct_1",
      hint: "Optional. An opaque reference to a connected account or credential the host resolves — never a token or secret. Leave empty to use the default connection.",
    },
  ],
  http_request: [
    {
      key: "method",
      label: "Method",
      control: "select",
      default: "GET",
      options: [
        { value: "GET", label: "GET" },
        { value: "POST", label: "POST" },
        { value: "PUT", label: "PUT" },
        { value: "PATCH", label: "PATCH" },
        { value: "DELETE", label: "DELETE" },
      ],
    },
    {
      key: "url",
      label: "URL",
      control: "line",
      required: true,
      placeholder: "https://api.example.com/v1  or  =item.url",
      hint: "The request URL. May be an `=`-expression, e.g. `=item.url`.",
    },
    {
      key: "headers",
      label: "Headers",
      control: "json",
      jsonShape: "object",
      placeholder: '{ "Authorization": "Bearer …" }',
      hint: "A JSON object of request headers.",
    },
    {
      key: "body",
      label: "Body",
      control: "json",
      jsonShape: "object-or-string",
      placeholder: '{ "hello": "world" }',
      hint: "A JSON object sent as the body, or a JSON string like `\"raw text\"`.",
    },
    {
      key: "connection_ref",
      label: "Connection",
      control: "line",
      placeholder: "http:acct_2",
      hint: "Optional. An opaque reference to a connected account or credential the host resolves — never a token or secret. Leave empty for an unauthenticated request.",
    },
  ],
  switch: [
    {
      key: "field",
      label: "Field",
      control: "line",
      placeholder: "e.g. status",
      hint: "A key on the first input item to branch on.",
    },
    {
      key: "expression",
      label: "Expression",
      control: "line",
      placeholder: "=item.score > 0.5",
      hint: "An `=`-expression to branch on. Give a field OR an expression.",
    },
  ],
  output_parser: [
    {
      key: "schema",
      label: "Schema",
      control: "json",
      placeholder: '{ "type": "object", "properties": { "name": { "type": "string" } } }',
      hint: "A JSON-Schema subset. Leave empty to pass the output through unchanged.",
    },
    {
      key: "auto_fix",
      label: "Auto-fix malformed output",
      control: "select",
      boolean: true,
      options: [
        { value: "", label: "Default (on)" },
        { value: "true", label: "On — repair to match the schema" },
        { value: "false", label: "Off" },
      ],
    },
  ],
  sub_workflow: [
    {
      key: "workflow_id",
      label: "Workflow to run",
      control: "workflow-ref",
      required: true,
      placeholder: "another workflow's id",
      hint: "The id of the workflow to run. It can't be this workflow's own id.",
    },
  ],
  transform: [
    {
      key: "set",
      label: "Set fields",
      control: "json",
      jsonShape: "object",
      placeholder: '{ "greeting": "=item.name" }',
      hint: "Optional. A JSON object of fields to add or overwrite on each item — each value is a literal or an `=`-expression resolved per item. Leave empty to pass items through unchanged.",
    },
  ],
};

/**
 * A static caption shown under a kind's fields — for what the form deliberately
 * does NOT collect. `switch` cases are the biggest trap: they read like config
 * but are the labels on the node's outgoing edges.
 */
export const NODE_CONFIG_NOTES: Record<string, string> = {
  switch:
    "Cases aren't set here — they're the labels on this node's outgoing edges. An unlabeled edge is the `default` branch.",
};

/**
 * Config keys the host rejects inside `config` (they are first-class node
 * fields). Dropped, never preserved, so a form's output can never carry one —
 * even if a hand-authored graph smuggled it in.
 */
export const RESERVED_CONFIG_KEYS: readonly string[] = [
  "on_error",
  "retry",
  "requires_approval",
  "schedule",
  "destination",
  "agent_ref",
];

/** The config fields for `kind`, or `[]` when it has no config form. */
export function configFieldSpecs(kind: string): readonly ConfigFieldSpec[] {
  return NODE_CONFIG_FIELDS[kind] ?? [];
}

/** Whether `kind` is one of the five that author config through a form. */
export function hasConfigForm(kind: string): boolean {
  return kind in NODE_CONFIG_FIELDS;
}

/** A fresh draft for `kind`: one entry per field, each at its default (or `""`). */
export function blankConfigDraft(kind: string): Record<string, string> {
  const draft: Record<string, string> = {};
  for (const spec of configFieldSpecs(kind)) draft[spec.key] = spec.default ?? "";
  return draft;
}

/** A single field's value, as a form string. */
function stringifyConfigValue(spec: ConfigFieldSpec, value: unknown): string {
  if (value === undefined || value === null) return "";
  // `json` fields always round-trip through JSON so a stored raw string
  // (a valid `body`) comes back quoted and re-parses to the same string.
  if (spec.control === "json") return JSON.stringify(value, null, 2);
  if (spec.boolean) return value === true ? "true" : value === false ? "false" : String(value);
  return typeof value === "string" ? value : String(value);
}

/**
 * Hydrate a saved node's `config` into a form draft (issue #259/#541).
 *
 * Known keys become form strings. **Every other key is preserved verbatim in
 * `extra`** — the anti-data-loss guard: an orchestrator-authored node can carry
 * keys this form has no control for (`execution`/`concurrency`/`inputs` on a
 * sub_workflow), and rebuilding the node from the visible controls alone would
 * silently delete them on the first save. Reserved keys are the one exception —
 * dropped, since the host would reject them anyway.
 */
export function configDraftFrom(
  kind: string,
  config: unknown,
): { draft: Record<string, string>; extra: Record<string, unknown> } {
  const draft = blankConfigDraft(kind);
  const extra: Record<string, unknown> = {};
  const specs = configFieldSpecs(kind);
  const byKey = new Map(specs.map((s) => [s.key, s]));

  if (config && typeof config === "object" && !Array.isArray(config)) {
    for (const [key, value] of Object.entries(config as Record<string, unknown>)) {
      const spec = byKey.get(key);
      if (spec) {
        draft[key] = stringifyConfigValue(spec, value);
      } else if (!RESERVED_CONFIG_KEYS.includes(key)) {
        extra[key] = value;
      }
    }
  }
  return { draft, extra };
}

/**
 * What {@link configFromDraft} answers: the node's `config` — `undefined` when
 * it would be empty, so the field is omitted from the body — or the reason it
 * could not be built.
 *
 * A value rather than a throw, because of issue #1006. The one caller assembles
 * every node inside its submit path; an exception escaping from there ran past
 * the `finally` that clears `submitting`, which gates both Cancel and the
 * dialog's own close handler. The operator was then locked in the dialog with
 * their unsaved graph, and the only way out — reloading the page — was the one
 * that lost it. A result the caller has to unwrap cannot skip a `finally`.
 */
export type ConfigFromDraft =
  | { ok: true; config: Record<string, unknown> | undefined }
  | { ok: false; error: string };

/**
 * Serialize a draft back into a node's `config`.
 *
 * Empty fields are dropped; `json` fields are parsed; the preserved `extra` bag
 * is re-merged verbatim.
 *
 * A malformed `json` field should never reach here — {@link configDraftProblem}
 * rejects it at submit — so `{ ok: false }` means the two disagreed, which is a
 * programmer error rather than user input. It is still reported as a value:
 * see {@link ConfigFromDraft} for why that distinction is load-bearing.
 */
export function configFromDraft(
  kind: string,
  draft: Record<string, string>,
  extra?: Record<string, unknown>,
): ConfigFromDraft {
  const config: Record<string, unknown> = {};
  for (const spec of configFieldSpecs(kind)) {
    const raw = (draft[spec.key] ?? "").trim();
    if (raw === "") continue;
    if (spec.control === "json") {
      try {
        config[spec.key] = JSON.parse(raw);
      } catch {
        return { ok: false, error: `${spec.label} must be valid JSON.` };
      }
    } else if (spec.boolean) {
      config[spec.key] = raw === "true";
    } else {
      config[spec.key] = raw;
    }
  }
  if (extra) {
    for (const [key, value] of Object.entries(extra)) {
      if (!RESERVED_CONFIG_KEYS.includes(key)) config[key] = value;
    }
  }
  return { ok: true, config: Object.keys(config).length > 0 ? config : undefined };
}

/**
 * One field's own problem, raised on blur. `null` when fine.
 *
 * An EMPTY field is never flagged here — "you haven't filled this in yet" is
 * nagging, not feedback; emptiness is {@link configDraftProblem}'s business at
 * submit. Only a malformed `json` field has a rule of its own.
 */
export function configFieldProblem(spec: ConfigFieldSpec, value: string): string | null {
  const raw = value.trim();
  if (raw === "") return null;
  if (spec.control === "json") {
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      return `${spec.label} must be valid JSON.`;
    }
    return jsonShapeProblem(spec, parsed);
  }
  return null;
}

/** A plain JSON object — not an array, not `null`, not a scalar. */
function isJsonObject(value: unknown): boolean {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Whether a parsed `json` field's VALUE fits the shape the engine key wants.
 * `null` when fine (or when the field declares no shape, i.e. any JSON goes).
 * Catches `[]`/numbers/booleans slipping into an `args`/`headers` object, and
 * anything but an object-or-string for an HTTP `body`.
 */
function jsonShapeProblem(spec: ConfigFieldSpec, parsed: unknown): string | null {
  switch (spec.jsonShape) {
    case "object":
      return isJsonObject(parsed) ? null : `${spec.label} must be a JSON object.`;
    case "object-or-string":
      return isJsonObject(parsed) || typeof parsed === "string"
        ? null
        : `${spec.label} must be a JSON object or a JSON string.`;
    default:
      return null;
  }
}

/** The submit-time message for a missing required field. */
function requiredMessage(kind: string, spec: ConfigFieldSpec): string {
  if (kind === "tool_call" && spec.key === "slug") {
    return "A tool call needs a tool slug — name the tool it runs.";
  }
  if (kind === "http_request" && spec.key === "url") {
    return "An HTTP request needs a URL.";
  }
  if (kind === "sub_workflow" && spec.key === "workflow_id") {
    return "A sub-workflow needs the id of the workflow to run.";
  }
  return `${spec.label} is required.`;
}

/**
 * The first submit-blocking problem with a config draft, or `null` when it is
 * postable. Pure, so the dialog's `validate()` and the unit tests share it.
 *
 * Covers: required fields, malformed JSON, the `switch` field-or-expression
 * rule, and a `sub_workflow` pointed at its own id.
 */
export function configDraftProblem(
  kind: string,
  selfId: string,
  draft: Record<string, string>,
): string | null {
  const specs = configFieldSpecs(kind);
  if (specs.length === 0) return null;

  for (const spec of specs) {
    if (spec.required && !(draft[spec.key] ?? "").trim()) {
      return requiredMessage(kind, spec);
    }
  }
  for (const spec of specs) {
    const problem = configFieldProblem(spec, draft[spec.key] ?? "");
    if (problem) return problem;
  }

  if (kind === "switch") {
    const field = (draft.field ?? "").trim();
    const expression = (draft.expression ?? "").trim();
    if (!field && !expression) {
      return "A switch needs a field or an expression to branch on.";
    }
    if (field && expression) {
      return "A switch can use a field or an expression, not both.";
    }
  }
  if (kind === "sub_workflow") {
    const wid = (draft.workflow_id ?? "").trim();
    if (wid && selfId.trim() && wid === selfId.trim()) {
      return "A sub-workflow can't call itself — point it at a different workflow's id.";
    }
  }
  return null;
}

/**
 * The console mirror of the host's write-time kind↔config rules, for the copilot
 * proposal path (issue #783). Returns the one actionable sentence the host would
 * refuse the write with, or `null` when the node is coherent for its kind.
 *
 * The source of truth is two host functions applied on every console-authored
 * graph (create AND update):
 *
 * - `required_config_problems` (`src/company/workflow_file.rs`): a `tool_call`
 *   needs `config.slug`, a `condition` needs `config.field`, an `http_request`
 *   needs `config.method` and `config.url`, a `switch` needs `config.field` OR
 *   `config.expression`, and a `sub_workflow` needs `config.workflow_id`.
 * - the `agent`-node arm of `validate_draft_against_record`
 *   (`src/company/workflow_create.rs`): an `agent` node must name a teammate in
 *   its first-class `agent` field (never in `config`).
 *
 * Deliberately NOT {@link configDraftProblem}: that is the *form* validator, and
 * it is stricter than the host in ways that would refuse a proposal the host
 * accepts — it requires a `switch` to set field XOR expression where the host
 * takes either, and it runs JSON-shape and blur rules the write path does not.
 * This checks exactly what the host checks, so a coherent proposal is never
 * turned away and an incoherent one is caught before the operator sees a diff
 * for a step Apply would bounce.
 *
 * Config **shape** only: whether a named tool is actually granted, or a named
 * teammate is really on the roster, stays the host's authority (a run-time gate
 * this console cannot see). This checks that the key is present, not that its
 * value resolves.
 */
export function nodeKindConfigProblem(node: {
  kind: string;
  agent?: string;
  config?: unknown;
}): string | null {
  const table =
    node.config && typeof node.config === "object" && !Array.isArray(node.config)
      ? (node.config as Record<string, unknown>)
      : undefined;
  const nonEmpty = (key: string): boolean => {
    const value = table?.[key];
    return typeof value === "string" && value.trim() !== "";
  };

  switch (node.kind) {
    case "agent":
      // `node` may be raw proposal JSON, so `agent` can be any type — guard the
      // string check rather than `.trim()` a non-string (which would throw and
      // take the whole validation down instead of refusing the one node).
      return typeof node.agent === "string" && node.agent.trim()
        ? null
        : "An agent step names no teammate — set its `agent` field to a roster member (not inside `config`).";
    case "tool_call":
      return nonEmpty("slug")
        ? null
        : 'A tool_call step sets no `config.slug` — put the tool\'s slug inside `config`, e.g. `"config": { "slug": "web_search" }`.';
    case "condition":
      return nonEmpty("field")
        ? null
        : "A condition step sets no `config.field` — put the boolean expression the branch tests inside `config`.";
    case "http_request":
      if (!nonEmpty("method")) {
        return 'An http_request step sets no `config.method` — name the HTTP method inside `config`, e.g. `"config": { "method": "GET" }`.';
      }
      return nonEmpty("url")
        ? null
        : "An http_request step sets no `config.url` — put the request URL inside `config`.";
    case "switch":
      return nonEmpty("field") || nonEmpty("expression")
        ? null
        : "A switch step names no discriminant — set `config.field` or `config.expression` inside `config`.";
    case "sub_workflow":
      return nonEmpty("workflow_id")
        ? null
        : "A sub_workflow step sets no `config.workflow_id` — name the workflow to run inside `config`.";
    default:
      return null;
  }
}
