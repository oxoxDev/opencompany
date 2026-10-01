// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it } from "vitest";

import { catalogPublisher, directoryServerName, mcpDisplayName } from "@/lib/mcp-registry";
import { McpServerIcon } from "@/views/connections/McpServerTable";

describe("catalogPublisher", () => {
  it("prefers the website's host", () => {
    expect(
      catalogPublisher({ qualifiedName: "com.notion/mcp", websiteUrl: "https://www.notion.so/help" }),
    ).toBe("notion.so");
  });

  it("reads a reverse-DNS namespace as a domain", () => {
    expect(catalogPublisher({ qualifiedName: "com.notion/mcp" })).toBe("notion.com");
    expect(catalogPublisher({ qualifiedName: "com.cloudflare.mcp/mcp" })).toBe("mcp.cloudflare.com");
  });

  it("reads a GitHub namespace as its owner", () => {
    expect(catalogPublisher({ qualifiedName: "io.github.github/github-mcp-server" })).toBe("github");
  });

  it("reads a scoped name as its scope", () => {
    expect(catalogPublisher({ qualifiedName: "@exa/exa" })).toBe("exa");
  });

  it("names nobody when the name encodes no owner", () => {
    expect(catalogPublisher({ qualifiedName: "standalone" })).toBeNull();
    expect(catalogPublisher({ qualifiedName: "x/y", websiteUrl: "not a url" })).toBeNull();
  });
});

describe("McpServerIcon", () => {
  function render(iconUrl: string | undefined) {
    const host = document.createElement("div");
    const root = createRoot(host);
    act(() => root.render(createElement(McpServerIcon, { iconUrl, name: "notion" })));
    const img = host.querySelector("img");
    act(() => root.unmount());
    return { img, text: host.textContent };
  }

  it("loads an inline image", () => {
    expect(render("data:image/png;base64,AAAA").img?.getAttribute("src")).toBe(
      "data:image/png;base64,AAAA",
    );
  });

  it("never asks the browser to fetch a remote address", () => {
    expect(render("https://icons.example/notion.png").img).toBeNull();
    expect(render("javascript:alert(1)").img).toBeNull();
  });
});

describe("directoryServerName", () => {
  it("slugs the shown name the way the host saves an install", () => {
    expect(directoryServerName("Notion")).toBe("notion");
    expect(directoryServerName("Atlassian Rovo MCP Server")).toBe("atlassian-rovo-mcp-server");
    expect(directoryServerName("inference.sh")).toBe("inference-sh");
    expect(directoryServerName("  — ")).toBe("");
  });
});

describe("mcpDisplayName", () => {
  it("shows a server under its own title, and its name when it has none", () => {
    expect(mcpDisplayName({ name: "notion", probedTitle: "Notion" })).toBe("Notion");
    expect(mcpDisplayName({ name: "notion", probedTitle: "  " })).toBe("notion");
    expect(mcpDisplayName({ name: "deepwiki" })).toBe("deepwiki");
  });
});
