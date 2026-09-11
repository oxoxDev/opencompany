// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { ChatHistoryMessageDto, ChatOutput } from "@/api/types";
import { fromHistory } from "@/lib/chat";
import { OutputLinkRow } from "@/views/room/MessageRow";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

function rehydratedOutputs(outputs: ChatOutput[]): ChatOutput[] {
  const entry: ChatHistoryMessageDto = {
    id: "42",
    channel: "writer",
    author: "writer",
    text: "Done.",
    atMillis: 1_700_000_000_000,
    mine: false,
    outputs,
  };
  return fromHistory([entry])[0]?.outputs ?? [];
}

function render(outputs: ChatOutput[]) {
  act(() => root.render(createElement(OutputLinkRow, { outputs: rehydratedOutputs(outputs) })));
}

describe("chat reply output links", () => {
  it("rehydrates one workspace output as a button link", () => {
    render([
      {
        kind: "workspace-node",
        targetId: "node-1",
        title: "launch-note.md",
      },
    ]);

    const link = container.querySelector("a");
    expect(link?.textContent).toContain("launch-note.md");
    expect(link?.getAttribute("href")).toBe("#/company/workspace/node-1");
    expect(container.querySelector("button")).toBeNull();
  });

  it("collapses several outputs behind a +N more control", () => {
    render([
      { kind: "workspace-node", targetId: "node-1", title: "first.md" },
      {
        kind: "artifact",
        targetId: "artifact-2",
        title: "Second draft",
        taskId: "task-7",
        version: 3,
      },
      { kind: "workspace-node", targetId: "node-3", title: "third.md" },
    ]);

    expect(container.querySelectorAll("a")).toHaveLength(1);
    const more = container.querySelector("button");
    expect(more?.textContent).toBe("+2 more");
    act(() => more?.click());
    expect(container.querySelectorAll("a")).toHaveLength(3);
    expect(container.querySelectorAll("a")[1]?.getAttribute("href")).toBe(
      "#/tasks/task-7?artifact=artifact-2&v=3",
    );
  });

  it("renders no row when the reply produced nothing", () => {
    render([]);
    expect(container.querySelector("[data-chat-output-links]")).toBeNull();
  });
});
