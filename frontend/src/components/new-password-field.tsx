import { useState } from "react";
import { Check, Copy, RefreshCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { generatePassword } from "@/lib/generate-password";

interface Props {
  /** The password as it stands; the field is controlled by its owner. */
  value: string;
  onChange: (value: string) => void;
  /** Form element id, so a label can name it. */
  id?: string;
  /** The label text. */
  label?: string;
  /** Submit on Enter, where the owner has a submit to run. */
  onEnter?: () => void;
  /** Whether to take focus on mount. */
  autoFocus?: boolean;
  /** A problem with the current value, rendered under the field. */
  problem?: string;
}

/**
 * The field a first admin sets their password in: pre-filled with a generated
 * one, shown in the clear, with a copy button and a way to roll another.
 *
 * Shown rather than masked on purpose. This is the moment the password is
 * *created*, not entered, and the one thing that can go wrong here is the
 * person not knowing what it is — a generated value they never saw is a
 * lockout waiting for the next visit. The copy button is for the same reason:
 * the next step is putting it somewhere safe, and a selection is fiddly.
 *
 * Editable, so somebody who wants their own password types over it. The
 * host's policy is length-only, and the owner reports a problem through
 * `problem` rather than this component guessing at the rule.
 */
export function NewPasswordField({
  value,
  onChange,
  id = "new-password",
  label = "Password",
  onEnter,
  autoFocus,
  problem,
}: Props) {
  const [copied, setCopied] = useState(false);

  function copy() {
    void navigator.clipboard?.writeText(value).then(
      () => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      },
      () => {
        // The clipboard refused (an insecure origin, a denied permission). The
        // value is on screen in the clear; there is nothing more to say.
      },
    );
  }

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <Label htmlFor={id}>{label}</Label>
        <div className="flex items-center gap-1">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-7 px-2 text-xs"
            onClick={() => {
              onChange(generatePassword());
              setCopied(false);
            }}
            data-testid="new-password-generate"
          >
            <RefreshCw className="mr-1 size-3" />
            Generate
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-7 px-2 text-xs"
            onClick={copy}
            disabled={!value}
            data-testid="new-password-copy"
          >
            {copied ? <Check className="mr-1 size-3" /> : <Copy className="mr-1 size-3" />}
            {copied ? "Copied" : "Copy"}
          </Button>
        </div>
      </div>
      <Input
        id={id}
        type="text"
        autoComplete="new-password"
        spellCheck={false}
        autoFocus={autoFocus}
        className="font-mono"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && onEnter) onEnter();
        }}
        data-testid="new-password"
      />
      {problem ? (
        <p className="text-xs text-destructive" data-testid="new-password-problem">
          {problem}
        </p>
      ) : (
        <p className="text-xs text-muted-foreground">
          Keep a copy — it&apos;s how you get back in. You can change it once you&apos;re
          signed in.
        </p>
      )}
    </div>
  );
}
