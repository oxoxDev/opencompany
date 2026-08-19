// Human copy for the instance settings the setup wizard writes.
//
// `GET /api/v1/setup` returns each field as its dotted `config.toml` key and
// nothing else — `workspace.max_blob_mb`, `openhuman_url`, `bind`. That is the
// right thing for the host to send: the key is what it writes, and inventing
// prose server-side would put product copy in a config resolver.
//
// So the console owns the words. Rendering the raw key as the label, which is
// what the wizard did first, turns a settings screen into a `.toml` file with
// input boxes — an operator reading `workspace.storage_quota_gb` has to already
// know the system to know what they are changing.
//
// The key is still shown, small and monospaced, under the label: whoever came
// here on purpose is often the person who will next edit that file by hand, and
// hiding the mapping would cost them more than the prose gains.

/** What an operator reads instead of the key. */
export interface FieldCopy {
  label: string;
  /** One line on what changing it does. Omitted where the label says it all. */
  hint?: string;
  /** Shown in the field's placeholder when nothing overrides the default. */
  unit?: string;
}

const COPY: Record<string, FieldCopy> = {
  brain_mode: {
    label: "How your teammates think",
    hint: "Hosted cognition, or a local runtime you point at yourself.",
  },
  api_url: {
    label: "TinyHumans API",
    hint: "Where hosted cognition is reached. The default is the managed endpoint.",
  },
  openhuman_url: {
    label: "OpenHuman runtime",
    hint: "Only used when you run the runtime yourself.",
  },
  bind: {
    label: "Address this host serves on",
    hint: "Loopback keeps it to this machine. Anything else exposes it to your network.",
  },
  public_url: {
    label: "Public address",
    hint: "The URL people reach this host at, when it sits behind a proxy or tunnel.",
  },
  "workspace.max_blob_mb": {
    label: "Largest file an agent may store",
    unit: "MB",
  },
  "workspace.storage_quota_gb": {
    label: "Total workspace size",
    unit: "GB",
  },
  github_token: {
    label: "GitHub token",
    hint: "Lets agents read and open pull requests on repositories you bind.",
  },
  tinyhumans_api_key: {
    label: "TinyHumans API key",
    hint: "What your teammates think with.",
  },
  auth_mode: {
    label: "Sign-in mode",
    hint: "Chosen with the cards above.",
  },
};

/**
 * The copy for `key`, falling back to a readable form of the key itself.
 *
 * A fallback rather than a throw: the host decides which fields exist, and a
 * build that grows one before this map does should render an imperfect label
 * rather than a blank screen. `assertEveryFieldHasCopy` in the tests is what
 * keeps the map honest for the fields the wizard actually lists.
 */
export function fieldCopy(key: string): FieldCopy {
  const known = COPY[key];
  if (known) return known;
  return { label: humanise(key) };
}

/** Whether the console has real copy for this key, rather than a fallback. */
export function hasFieldCopy(key: string): boolean {
  return key in COPY;
}

/** `workspace.max_blob_mb` → `Workspace max blob mb`. Last resort only. */
function humanise(key: string): string {
  const words = key.replace(/[._]/g, " ").trim();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/**
 * What the field's placeholder should say when the operator has typed nothing.
 *
 * `set by default` — the old text — described the *mechanism* and told an
 * operator nothing about the state they are in. These say what is true: the
 * value in force, where it came from, or that a secret is already stored.
 */
export function fieldPlaceholder(field: {
  layer: string;
  value: string | null;
  secret: boolean;
  key: string;
}): string {
  if (field.secret) return field.value === null ? "Not set" : "Stored — type to replace";
  if (field.value) return field.value;
  const unit = fieldCopy(field.key).unit;
  const suffix = unit ? ` (${unit})` : "";
  switch (field.layer) {
    case "default":
      return `Using the default${suffix}`;
    case "manifest":
      return `From this company's manifest${suffix}`;
    case "env":
      return `Set by the environment${suffix}`;
    default:
      return `Using the default${suffix}`;
  }
}
