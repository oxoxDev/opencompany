import { Building2, MessageSquareWarning, PanelLeftClose, PanelLeftOpen } from "lucide-react";

import type { CompanyStatus } from "@/api/types";
import type { View } from "@/components/app-shell";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from "@/components/ui/sidebar";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { DiscordIcon } from "@/components/discord-icon";
import { lifecycle } from "@/lib/language";
import { DISCORD_INVITE_URL } from "@/lib/links";
import { cn } from "@/lib/utils";

// The lifecycle row carries its state in the label as well as the dot — a
// coloured dot alone puts the whole signal on hue, which a colourblind reader
// or a glance at the collapsed rail can miss.
const TONE_DOT: Record<string, string> = {
  live: "bg-status-done",
  idle: "bg-status-blocked",
  stopped: "bg-status-failed",
};

const TONE_TEXT: Record<string, string> = {
  live: "text-status-done-text",
  idle: "text-status-blocked-text",
  stopped: "text-status-failed-text",
};

// Discord's brand blurple, lifted a step in dark mode so it clears the
// sidebar's surface instead of sinking into it. Named tokens rather than raw
// hex — the colour is deliberately not ours, and saying so in the token name
// is what stops it being "fixed" into the palette later. See `--brand-discord`
// in index.css.
const DISCORD_BLURPLE =
  "text-(--brand-discord-on-light) dark:text-(--brand-discord-on-dark)";

/**
 * A sidebar row at rest: dimmed until you reach for it.
 *
 * The sidebar is standing furniture, on screen behind every view — holding the
 * whole list at full strength makes ten equal-weight rows compete with the
 * content beside them. Hover, keyboard focus, and the active row all come back
 * to full, so nothing is ever dimmed at the moment you are using it.
 */
// `data-active` is a bare boolean attribute on these buttons, not
// `data-active="true"` — match it the same way the sidebar's own styles do.
export const RESTING_ROW =
  "opacity-60 transition-opacity hover:opacity-100 focus-visible:opacity-100 data-active:opacity-100";

interface Props {
  /** The company's lifecycle, shown as a dot + label. */
  lifecycleState: string;
  /**
   * Issue #86: whether the governance kill switch is engaged. Overrides the
   * lifecycle label, because a stopped company still reports `running` and this
   * row is the one piece of company state on screen behind every view.
   */
  emergencyPaused?: boolean;
  /** Every company this operator can reach, for the switcher. */
  companies: CompanyStatus[];
  activeCompany: string | null;
  onSwitchCompany: (id: string) => void;
  onBackToPicker?: () => void;
  /** The active view, so the Feedback row can show as selected. */
  view: View;
  onNavigate: (view: View) => void;
}

/**
 * The sidebar's standing controls.
 *
 * No page carries a header of its own any more, so what is left of the old top
 * bar lives here: the company's state and the switcher. Collapsing is NOT one
 * of these — it is chrome rather than a destination, so it is a button in the
 * sidebar's header beside the host switcher (`SidebarCollapseButton`, below)
 * rather than a row in either menu. Theming and flagging are deliberately
 * absent — Settings owns both, under Appearance and "Something off?", and a
 * second entry point would just be two places to keep in step.
 */
export function SidebarControls({
  lifecycleState,
  emergencyPaused,
  companies,
  activeCompany,
  onSwitchCompany,
  onBackToPicker,
  view,
  onNavigate,
}: Props) {
  const { label, tone } = lifecycle(lifecycleState, emergencyPaused);

  return (
    <SidebarMenu>
      {/* Company state. Not a control — `cursor-default` and inert, so it does
          not read as something to press. */}
      <SidebarMenuItem>
        <SidebarMenuButton
          tooltip={label}
          className={cn("cursor-default font-medium hover:bg-transparent", TONE_TEXT[tone])}
        >
          <span className="flex size-4 items-center justify-center">
            <span
              className={cn(
                "size-2 rounded-full",
                TONE_DOT[tone],
                tone === "live" && "animate-pulse",
              )}
            />
          </span>
          <span>{label}</span>
        </SidebarMenuButton>
      </SidebarMenuItem>

      {/* Feedback is a destination like any nav item, but it belongs with the
          standing controls at the bottom rather than in the working nav. */}
      <SidebarMenuItem>
        <SidebarMenuButton
          tooltip="Feedback"
          isActive={view === "feedback"}
          onClick={() => onNavigate("feedback")}
          className={RESTING_ROW}
        >
          <MessageSquareWarning />
          <span>Feedback</span>
        </SidebarMenuButton>
      </SidebarMenuItem>

      {/* Switching companies lived in the header block that was removed. It
          is a real capability, so it moves here rather than disappearing —
          hidden entirely when there is only one company to be in. */}
      {(companies.length > 1 || onBackToPicker) && (
        <SidebarMenuItem>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={<SidebarMenuButton tooltip="Switch company" className={RESTING_ROW} />}
            >
              <Building2 />
              <span>Switch company</span>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" side="right">
              {companies.map((c) => (
                <DropdownMenuItem
                  key={c.id}
                  onClick={() => onSwitchCompany(c.id)}
                  className={c.id === activeCompany ? "font-medium" : undefined}
                >
                  {c.name}
                </DropdownMenuItem>
              ))}
              {onBackToPicker && (
                <DropdownMenuItem onClick={onBackToPicker}>All companies…</DropdownMenuItem>
              )}
            </DropdownMenuContent>
          </DropdownMenu>
        </SidebarMenuItem>
      )}

      <SidebarMenuItem>
        {/* Deliberately NOT `RESTING_ROW`.

            The resting dim is `opacity-60`, which is safe for a row of
            near-white text — 16.87:1 becomes 6.60:1 — and destroys a
            mid-tone hue: the blurple measures 6.36:1 at full strength and
            3.04:1 dimmed, under the 4.5:1 a 14px label needs. Recovering
            that inside the dim would mean lightening the blurple by five
            steps, at which point it is a pale lavender and no longer reads
            as Discord's colour at all.

            So this row is not dimmed. Its hue already sets it apart from the
            nav above, without help from the property doing the damage. */}
        <SidebarMenuButton
          tooltip="Join our Discord"
          className={cn(
            DISCORD_BLURPLE,
            "hover:text-(--brand-discord-on-light) dark:hover:text-(--brand-discord-on-dark)",
          )}
          render={<a href={DISCORD_INVITE_URL} target="_blank" rel="noreferrer" />}
        >
          <DiscordIcon className="size-4" />
          <span>Join our Discord</span>
        </SidebarMenuButton>
      </SidebarMenuItem>

    </SidebarMenu>
  );
}

/**
 * Show or hide the sidebar. A button in the header, not a row in the nav.
 *
 * ## Why it is not a row (issue #1177)
 *
 * It used to be a `SidebarMenuButton` — full width, icon then label, `h-8`,
 * `bg-sidebar-accent` on hover — sitting directly under the host switcher and
 * directly above Overview. That is the nav row shape exactly, so the eye filed
 * it as the first destination in the list. It is not a destination: everything
 * else in that column takes you somewhere, and this one changes the chrome and
 * leaves you where you are.
 *
 * Colouring it differently would not have fixed that; the shape is what says
 * "row". So it stops using the row primitive altogether and becomes the
 * console's ordinary icon button, in the sidebar's header — which is the part
 * of the column that talks about the panel rather than about the company.
 * `SidebarContent` below it is the destinations, and the header/content
 * boundary now means something.
 *
 * ## Why it does not crowd the host switcher (issue #1174)
 *
 * The switcher beside it is `h-12`, carries a filled glyph, a two-line
 * nameplate and the cross-host status dot. This is 28px, ghost, and dimmed at
 * rest. They also sit in separate elements, so hovering one never lights the
 * other — which is what stops the pair reading as a single control with a
 * chevron at one end and a panel glyph at the other.
 *
 * ## The collapsed rail
 *
 * The rail is `--sidebar-width-icon` (3rem) and `SidebarHeader` is `p-2`, so
 * there are 32px of content box — exactly the switcher's glyph, and no room
 * for anything beside it. The header row therefore becomes a column on the
 * rail (see `app-shell.tsx`), and this button grows to `size-8` there so it
 * lands on the same 32px rhythm as every nav icon below it.
 *
 * It deliberately drops the `bg-primary` fill it used to take when collapsed.
 * That fill existed to make it findable in a column of identical nav icons; up
 * here it has only the switcher for company, and the switcher's glyph is
 * *already* a filled primary square. Two of those stacked would read as one
 * control, which is the failure the paragraph above exists to prevent.
 */
export function SidebarCollapseButton() {
  const { toggleSidebar, state, isMobile } = useSidebar();
  // `state` tracks the DESKTOP open flag; the sheet has its own (`openMobile`).
  // Reading it unguarded labels an open sheet "Expand sidebar" whenever the
  // desktop state happens to be collapsed — which, since issue #1176 stopped
  // the sidebar auto-collapsing, is now a state an operator can leave behind
  // and come back to on a phone.
  const collapsed = !isMobile && state === "collapsed";
  const label = collapsed ? "Expand sidebar" : "Collapse sidebar";
  const Icon = collapsed ? PanelLeftOpen : PanelLeftClose;

  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            // The accessible name, and the only name this control has — an
            // icon-only button with no label is otherwise announced as
            // "button". The tooltip says the same words, but a tooltip is a
            // visual affordance and cannot be relied on for the name.
            aria-label={label}
            // Deliberately NO `aria-expanded`, and not an oversight.
            //
            // The name already carries the state: it says "Collapse sidebar"
            // while the column is showing and "Expand sidebar" once it is a
            // rail, so a reader is told what pressing does and, by the change,
            // what happened. `aria-expanded` on top of that announces the
            // state twice ("Expand sidebar, collapsed") — and `ghost` styles
            // the attribute as "the popup under me is open", which is what it
            // means on the dropdown triggers that variant was written for. On
            // this button it painted a pressed chip for as long as the sidebar
            // was open, and Tailwind sorts `aria-expanded:` after `hover:`, so
            // overriding the chip also swallowed the hover feedback. A second
            // channel saying the same thing is not worth either.
            data-testid="sidebar-collapse"
            onClick={toggleSidebar}
            className={cn(
              // The same resting dim as the rows below, reached through the
              // ink's alpha rather than `RESTING_ROW`'s `opacity-60`: opacity
              // dims the whole box, focus ring included, and the ring on an
              // unlabelled button is the only thing saying where the keyboard
              // is. (`RESTING_ROW` also carries `data-active:opacity-100`,
              // which is a nav row's business and never this one's.)
              "shrink-0 text-sidebar-foreground/60",
              // Three classes replacing exactly one of `ghost`'s each, so
              // tailwind-merge drops the original rather than leaving the two
              // to race: `hover:bg-muted`, `hover:text-foreground` and
              // `dark:hover:bg-muted/50`. The muted tint is tuned against the
              // canvas; this button is on the sidebar's surface, which is a
              // different rung and moving again in issue #1178. The accent is
              // also what every row in this column already hovers to.
              "hover:bg-sidebar-accent hover:text-sidebar-accent-foreground dark:hover:bg-sidebar-accent",
              "focus-visible:ring-sidebar-ring/50",
              // 28px beside a 48px nameplate, 32px on the rail. See the note
              // on the collapsed rail above.
              "group-data-[collapsible=icon]:size-8",
            )}
          />
        }
      >
        <Icon />
      </TooltipTrigger>
      {/*
        The raw tooltip primitive rather than `SidebarMenuButton`'s `tooltip`
        prop, which renders its content with `hidden={state !== "collapsed"}`.
        That is right for a nav row — expanded, the row already carries its
        label — and wrong here: this button is icon-only in BOTH states, and
        expanded is the state in which a reader has never seen the word.

        `side="right"` in both states, matching every other tooltip in this
        column, and the one side that is clear of the sidebar either way.
      */}
      <TooltipContent side="right">{label}</TooltipContent>
    </Tooltip>
  );
}

