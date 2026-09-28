import { describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { fetchRecentRuns, fetchRun } from "@/api/observatory";

/**
 * The Observatory's run queries select `episodeId` / `roundRevision`, which a
 * host predating desk episodes does not have in its schema. GraphQL validates
 * the selection before resolving, so on such a host the **whole** query is
 * refused — and the Observatory must not go blank for it. The read retries
 * without the two fields, once, and only for that refusal.
 */

function refusing(field: string) {
  return { errors: [{ message: `Unknown field "${field}" on type "AgentRun"` }] };
}

describe("observatory runs on a host without episode fields", () => {
  it("retries the list without the fields when the schema refuses them", async () => {
    const graphqlRequest = vi
      .fn()
      .mockResolvedValueOnce(refusing("episodeId"))
      .mockResolvedValueOnce({ data: { company: { agentRuns: [{ id: "r1" }] } } });
    const client = { graphqlRequest } as unknown as OpenCompanyClient;
    const runs = await fetchRecentRuns(client, "acme", 10);
    expect(runs).toEqual([{ id: "r1" }]);
    expect(graphqlRequest).toHaveBeenCalledTimes(2);
    const [first] = graphqlRequest.mock.calls[0] as [string];
    const [second] = graphqlRequest.mock.calls[1] as [string];
    expect(first).toContain("episodeId");
    expect(second).not.toContain("episodeId");
    expect(second).not.toContain("roundRevision");
  });

  it("asks once, with the fields, on a host that has them", async () => {
    const graphqlRequest = vi.fn().mockResolvedValue({ data: { company: { agentRun: { id: "r1", episodeId: "ep" } } } });
    const client = { graphqlRequest } as unknown as OpenCompanyClient;
    expect(await fetchRun(client, "acme", "r1")).toEqual({ id: "r1", episodeId: "ep" });
    expect(graphqlRequest).toHaveBeenCalledTimes(1);
  });

  it("does not retry any other refusal", async () => {
    const graphqlRequest = vi.fn().mockResolvedValue({ errors: [{ message: "forbidden" }] });
    const client = { graphqlRequest } as unknown as OpenCompanyClient;
    await expect(fetchRecentRuns(client, "acme")).rejects.toThrow();
    expect(graphqlRequest).toHaveBeenCalledTimes(1);
  });
});
