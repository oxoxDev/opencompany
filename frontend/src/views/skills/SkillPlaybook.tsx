// A skill's SKILL.md on its detail page: what the agents actually read, and —
// where the host allows it — the one place to change it.
//
// The document was unaddressable until `GET …/skills/{slug}/doc` existed, which
// is why the console could only ever grey out Edit. Shown read-only first and
// edited in place rather than in a dialog: the text IS the skill, so a modal
// that covers the page it belongs to hides the thing being judged.
//
// `editable` comes from the host, never from the row's `source`. A skill
// authored in the repository ships resource files beside its document that a
// stored delta cannot carry, so the host refuses the write and says so here.

import { useEffect, useState } from "react";

import type { OpenCompanyClient } from "@/api/client";
import { getSkillDoc, setSkillDoc } from "@/api/skills";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";

/** Why the document is shown but cannot be changed from here. */
const REPO_AUTHORED =
  "This skill is authored in the repository, so it is read-only here.";

export function SkillPlaybook({
  client,
  company,
  slug,
  canManage,
  onSaved,
}: {
  client: OpenCompanyClient;
  company: string | null;
  slug: string;
  /** Whether this viewer may change what the company's agents read. */
  canManage: boolean;
  /** Refetch the skills list after a write — the row's name and description
      come from this document. */
  onSaved: () => void;
}) {
  const [loaded, setLoaded] = useState<{
    markdown: string;
    editable: boolean;
  } | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  // `null` is "not editing". A separate draft rather than editing `loaded` in
  // place, so Cancel has something to go back to and a refetch cannot silently
  // overwrite a half-typed rewrite.
  const [draft, setDraft] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setLoaded(null);
    setFailed(null);
    setDraft(null);
    setProblem(null);
    void getSkillDoc(client, company, slug)
      .then((doc) => {
        if (live) setLoaded({ markdown: doc.markdown, editable: doc.editable });
      })
      .catch((e: unknown) => {
        if (live) {
          setFailed(e instanceof Error ? e.message : "the host refused it");
        }
      });
    return () => {
      live = false;
    };
  }, [client, company, slug]);

  async function save() {
    if (draft === null) return;
    setSaving(true);
    setProblem(null);
    try {
      await setSkillDoc(client, company, slug, draft);
      setLoaded((was) => (was ? { ...was, markdown: draft } : was));
      setDraft(null);
      onSaved();
    } catch (e) {
      setProblem(e instanceof Error ? e.message : "the host refused it");
    } finally {
      setSaving(false);
    }
  }

  return (
    <section className="space-y-3" aria-label="Playbook">
      <div className="flex items-center justify-between gap-2">
        <h4 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          Playbook
        </h4>
        {loaded?.editable && canManage && draft === null && (
          <Button
            size="sm"
            variant="outline"
            onClick={() => setDraft(loaded.markdown)}
            data-testid="skill-doc-edit"
          >
            Edit
          </Button>
        )}
      </div>

      {failed !== null ? (
        <Alert variant="destructive" data-testid="skill-doc-failed">
          <AlertDescription>{failed}</AlertDescription>
        </Alert>
      ) : loaded === null ? (
        <Skeleton className="h-40 rounded-lg" data-testid="skill-doc-loading" />
      ) : draft === null ? (
        <>
          <pre
            className="max-h-96 overflow-auto rounded-lg border bg-muted/40 p-3 font-mono text-xs whitespace-pre-wrap"
            data-testid="skill-doc-text"
          >
            {loaded.markdown}
          </pre>
          {!loaded.editable && (
            <p
              className="text-xs text-muted-foreground"
              data-testid="skill-doc-read-only"
            >
              {REPO_AUTHORED}
            </p>
          )}
        </>
      ) : (
        <>
          <Textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            disabled={saving}
            spellCheck={false}
            rows={20}
            aria-label="Playbook"
            className="min-h-64 font-mono text-xs"
            data-testid="skill-doc-editor"
          />
          {problem && (
            <Alert variant="destructive" data-testid="skill-doc-problem">
              <AlertDescription>{problem}</AlertDescription>
            </Alert>
          )}
          <div className="flex justify-end gap-2">
            <Button
              variant="ghost"
              size="sm"
              disabled={saving}
              onClick={() => {
                setDraft(null);
                setProblem(null);
              }}
              data-testid="skill-doc-cancel"
            >
              Cancel
            </Button>
            <Button
              size="sm"
              disabled={saving || draft === loaded.markdown}
              onClick={() => void save()}
              data-testid="skill-doc-save"
            >
              Save
            </Button>
          </div>
        </>
      )}
    </section>
  );
}
