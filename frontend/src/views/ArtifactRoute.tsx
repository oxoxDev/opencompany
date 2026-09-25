// One published deliverable, on its own page.
//
// `#/artifacts/<artifactId>?v=<n>`. The chat row that offers a deliverable
// links straight here, rather than at the card `publish_artifact` minted to
// satisfy the artifact store's `(task_id, source)` identity — see
// `cardOnlyCarriesAnArtifact` in `views/room/MessageRow.tsx` for the other half
// of that, which stops the same row offering a second door to the same thing.
//
// Deliberately thin: `ArtifactDetail` is the same component the card's
// Artifacts tab mounts, so a deliverable reads identically wherever it is
// opened from and there is no second renderer to drift.

import { useCallback, useEffect, useState } from "react";

import type { OpenCompanyClient } from "@/api/client";
import { getArtifact, type ArtifactView } from "@/api/artifacts";
import { PageHeader } from "@/components/page-header";
import { useHashParam } from "@/hooks/use-hash-param";
import { ArtifactDetail } from "@/views/ArtifactsTab";

export function ArtifactRoute({
  client,
  company,
  artifactId,
}: {
  client: OpenCompanyClient;
  company: string | null;
  artifactId: string;
}) {
  const [artifact, setArtifact] = useState<ArtifactView | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The revision the link arrived on, as `artifactHref` writes it. Absent means
  // "follow the newest", which is what an ordinary open has always done.
  const [pinned] = useHashParam("v");

  const load = useCallback(() => {
    if (!artifactId) return;
    let live = true;
    setError(null);
    getArtifact(client, company, artifactId)
      .then((next) => {
        if (live) setArtifact(next);
      })
      .catch((cause: unknown) => {
        if (live) setError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      live = false;
    };
  }, [client, company, artifactId]);

  useEffect(() => load(), [load]);

  if (!artifactId) return null;

  // Every state draws the heading (#1785): a page that titles itself only once
  // it has loaded leaves the reader on an unnamed screen for as long as the
  // read takes, and an error state with no title is a page that never says
  // what failed to open.
  if (error) {
    return (
      <>
        <PageHeader title="Deliverable" description="This deliverable could not be opened." />
        <div className="px-6 text-sm text-muted-foreground">{error}</div>
      </>
    );
  }

  if (!artifact) {
    return (
      <>
        <PageHeader title="Deliverable" description="Opening…" />
      </>
    );
  }

  const pinnedVersion = pinned !== null && Number.isFinite(Number(pinned)) ? Number(pinned) : null;

  return (
    <>
      <PageHeader title={artifact.title} description="Published deliverable" />
      <ArtifactDetail
      client={client}
      company={company}
      artifact={artifact}
      pinnedVersion={pinnedVersion}
      // No list behind this page to go back to, so Back is the browser's.
      onBack={() => window.history.back()}
      onLeavePin={() => undefined}
      onAppended={(next) => setArtifact(next)}
      onRefresh={() => load()}
      onEditingChange={() => undefined}
      />
    </>
  );
}
