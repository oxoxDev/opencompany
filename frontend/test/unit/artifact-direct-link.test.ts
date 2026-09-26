import { describe, expect, it } from "vitest";

import { artifactHref, artifactPageHref, cardHref } from "@/lib/task-output";

/**
 * A published deliverable is addressed as itself, not through its card.
 *
 * `publish_artifact` mints a card because an `ArtifactRecord`'s identity is
 * `(task_id, source)` and the store will not take an artifact without a task
 * (`ports/artifacts.rs`). That card is a storage requirement, not a piece of
 * work — and routing the chat row's link through it made an operator open a
 * board item to read the thing the row was already offering them.
 *
 * The task id stays in the signature because every call site has it and the
 * store still keys identity on it; it simply no longer decides the address.
 */
describe("artifactPageHref", () => {
  it("addresses the artifact, not the card that carries it", () => {
    const href = artifactPageHref("artifact-9", 3);
    expect(href).toBe("#/artifacts/artifact-9?v=3");
    expect(href.startsWith(cardHref("task-1"))).toBe(false);
  });

  it("carries the revision the link arrived on", () => {
    expect(artifactPageHref("a", 1)).toContain("v=1");
    expect(artifactPageHref("a", 12)).toContain("v=12");
  });

  it("escapes an id that would otherwise break the address", () => {
    expect(artifactPageHref("a/b?c", 1)).toBe("#/artifacts/a%2Fb%3Fc?v=1");
  });

  /**
   * The card's own address is deliberately untouched. The board's primary
   * link and a workflow run's file list are both looking AT a task and want
   * its outputs in place; only the chat row, which is not, goes direct.
   */
  it("leaves the card's Artifacts tab address alone", () => {
    expect(artifactHref("t-1", "a-1", 3)).toBe("#/tasks/t-1?artifact=a-1&v=3");
  });
});
