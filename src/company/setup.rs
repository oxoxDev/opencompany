//! First-run company setup: the curated rosters, and the rules that keep a
//! proposed roster sane.
//!
//! A brand-new company has no roster, and until now the console papered over
//! that with a fabricated twelve-agent starter team that existed only in the
//! browser. First-run setup replaces it: three questions, then four to six
//! agents actually created on the host. See
//! `docs/spec/runtime/company-setup.md`.
//!
//! ## Why the templates live here and not in the harness
//!
//! Everything in this module is deterministic and model-free, and that is the
//! point. `src/harness/` is entirely behind the non-default `openhuman`
//! feature, which CI's default lane never compiles — so a template table that
//! lived there would ship untested.
//!
//! The templates are **not** what a company normally gets. When a model is
//! wired it designs the team from the operator's own answers
//! (`crate::harness::roster_build`), and these curated rosters do two narrower
//! jobs:
//!
//! * **The floor.** No credential, a timeout, an unreadable answer — every
//!   failure lands here, so a company with no API key still gets a real
//!   industry team rather than an empty page. That is what makes the
//!   never-strand rule (decision D3) cheap to honour.
//! * **A quality bar.** The matched roster goes into the prompt as a reference
//!   for naming and phrasing, so a generated team reads like a written one.
//!
//! [`validate_roster`] is the other half, and the load-bearing one: it applies
//! the same bounds to a generated roster and a curated one, so the rules that
//! keep a team workable are a boundary rather than a request in a prompt.

use serde::{Deserialize, Serialize};

/// What the operator told us during setup.
///
/// Stored on the [`CompanyRecord`](crate::ports::types::CompanyRecord) so Phase
/// 2 (workflows) can build from answers already given rather than asking a
/// second time. Three free-text fields on purpose: people describe a business
/// in sentences, and a picker with twelve checkboxes collects less.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupAnswers {
    /// "What kind of company are you setting up?"
    #[serde(default)]
    pub industry: String,
    /// "What team do you need?" — free text alongside the pre-ticked roster.
    #[serde(default)]
    pub team_hint: String,
    /// "What are you trying to automate?" — the answer that becomes each
    /// agent's mandate in Phase 1, and the workflows in Phase 2.
    #[serde(default)]
    pub automate: String,
}

/// One agent a setup pass proposes. Not yet created — the console turns each of
/// these into a `POST {scope}/team` call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedAgent {
    /// The short display name, e.g. `Meta Ads`.
    pub name: String,
    /// The full title, e.g. `Meta Ads Specialist`.
    pub role: String,
    /// What this agent owns, in the operator's terms.
    pub description: String,
    /// The shape of work this teammate does, which is what decides its tool
    /// belt. See [`AgentFocus`]. Absent means "inherit the company belt", which
    /// is what every setup-built agent did before focus existed.
    #[serde(default, deserialize_with = "focus_from_wire")]
    pub focus: Option<AgentFocus>,
}

/// A curated agent inside a [`RosterTemplate`]. Static so the table costs no
/// allocation until a template is actually chosen.
#[derive(Clone, Copy, Debug)]
pub struct TemplateAgent {
    pub name: &'static str,
    pub role: &'static str,
    pub description: &'static str,
    /// The belt this curated teammate needs. Declared here rather than derived
    /// from the role, so the fallback team is scoped exactly as a designed one
    /// is — an operator with no credential must not end up with the *wider*
    /// company.
    pub focus: AgentFocus,
}

/// The shape of work a teammate does, and the only thing that decides its tool
/// belt.
///
/// ## Why the model names a job shape and never a tool
///
/// A setup roster is authored by a model reading free text a stranger typed, and
/// tool grants are a permission boundary. Letting the answer name grants
/// directly would put `[tools]` inside the blast radius of the prompt — the one
/// place a hostile "what do you do?" could pay off. A closed enum means the
/// worst a hostile answer achieves is the wrong belt from a list of four, all of
/// which the host wrote.
///
/// ## Why this exists at all
///
/// [`manifest_from_setup`] builds its manifest from a name-only base, so
/// `[tools]` took [`Tools::default`](crate::company::Tools) — the globals
/// baseline `["*", "media", "composio"]` — and every agent left `tools` empty,
/// which [`agent_effective_grants`](crate::runtime::builder) reads as *inherit
/// the lot*. So each teammate a first-run operator created held shell, code,
/// web, subagent, files, docs, **media** (which spends real money) and
/// **composio** (which reaches per-tenant credentials), for a company they had
/// described in three sentences.
///
/// The globals teammates next to them already do the opposite, and say why in
/// `globals/agents/researcher.toml`: a request is intersected with
/// `[tools].allow`, so naming one *can only ever narrow*. These belts are that
/// file's, verbatim, for exactly that reason — the strings are already exercised
/// in every company rather than invented here.
///
/// `search` is deliberately absent from every belt even though the globals
/// researcher names it: it bills per call, and a team nobody has met yet should
/// not arrive holding a spend authority. A company that wants it grants it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentFocus {
    /// Finds things out. Reads the workspace without writing it, and browses.
    Research,
    /// Produces the written work. Writes the workspace; no web.
    Writing,
    /// Keeps work moving. Same belt as [`Writing`](Self::Writing) today.
    Operations,
    /// Measures and reports. Writes the workspace, and browses to source the
    /// numbers.
    Analysis,
}

impl AgentFocus {
    /// Every focus, so a test can quantify over the whole vocabulary rather
    /// than over the four a reader happened to remember.
    pub const ALL: [Self; 4] = [
        Self::Research,
        Self::Writing,
        Self::Operations,
        Self::Analysis,
    ];

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Research => "research",
            Self::Writing => "writing",
            Self::Operations => "operations",
            Self::Analysis => "analysis",
        }
    }

    /// The focus this string names, or `None`.
    ///
    /// Unknown is `None` rather than an error on purpose: this parses model
    /// output, and a model that invents `"marketing"` should cost that teammate
    /// its narrowing, not cost the operator the whole roster. `None` is the
    /// pre-focus behaviour, which is worse but never broken.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "research" => Some(Self::Research),
            "writing" => Some(Self::Writing),
            "operations" => Some(Self::Operations),
            "analysis" => Some(Self::Analysis),
            _ => None,
        }
    }

    /// This focus's tool belt.
    ///
    /// `Writing` and `Operations` return the same list today, and that is not an
    /// oversight: they differ in mandate and in what the prompt routes to them,
    /// not in the tools they need. Keeping them distinct is what lets the belts
    /// diverge later without re-deciding which agents are which.
    pub fn tools(self) -> Vec<String> {
        let belt: &[&str] = match self {
            Self::Research => &["workspace.read", "docs.*", "files.*", "web.*"],
            Self::Writing | Self::Operations => &["workspace.*", "docs.*", "files.*"],
            Self::Analysis => &["workspace.*", "docs.*", "files.*", "web.*"],
        };
        belt.iter().map(|t| (*t).to_string()).collect()
    }
}

/// The belt for an optional focus. An unreadable or absent one gets the
/// **narrowest working belt**, never an empty list.
///
/// ## This failed open, and a prompt-injection test found it
///
/// It returned `Vec::new()` for `None`, reasoning that an unknown focus should
/// degrade to the pre-focus behaviour — "worse, but never broken". That was the
/// wrong default for a permission boundary, and it inverted the whole control:
/// an empty `tools` list is read as *inherit the company belt* by
/// [`agent_effective_grants`](crate::runtime::builder), and a setup-built
/// company's belt is the globals default `["*", "media", "composio"]`. So an
/// **invalid** focus produced a wider agent than any valid one, and anything able
/// to influence that string — the operator's own free text reaches a model that
/// writes it — escaped the narrowing simply by being unrecognisable.
///
/// [`WRITING`](AgentFocus::Writing)'s belt is the floor instead: the workspace,
/// documents and files. A teammate that lands there can still do its work, and
/// no unrecognised value can ever buy more authority than a recognised one.
/// Fail closed, then, in the only direction that matters — the failure mode is a
/// teammate that cannot browse, not one holding a spend authority.
pub fn tools_for_focus(focus: Option<AgentFocus>) -> Vec<String> {
    focus.unwrap_or(AgentFocus::Writing).tools()
}

/// Reads a focus off the wire, treating anything unrecognised as absent.
///
/// The derived `Option<AgentFocus>` would *fail* on an unknown string and take
/// the surrounding roster down with it. See [`AgentFocus::from_wire`].
fn focus_from_wire<'de, D>(deserializer: D) -> Result<Option<AgentFocus>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.as_deref().and_then(AgentFocus::from_wire))
}

/// A hand-written starting roster for one kind of business.
#[derive(Clone, Copy, Debug)]
pub struct RosterTemplate {
    /// Stable identifier, returned to the console and worth logging.
    pub key: &'static str,
    /// Human label, e.g. `E-commerce`.
    pub label: &'static str,
    /// Lowercase substrings that select this template. Matched against the
    /// answers, not against a fixed vocabulary the operator has to guess.
    pub keywords: &'static [&'static str],
    pub agents: &'static [TemplateAgent],
}

impl RosterTemplate {
    /// This template's agents as owned, proposal-shaped rows.
    pub fn proposed(&self) -> Vec<ProposedAgent> {
        self.agents
            .iter()
            .map(|a| ProposedAgent {
                name: a.name.to_string(),
                role: a.role.to_string(),
                description: a.description.to_string(),
                focus: Some(a.focus),
            })
            .collect()
    }
}

/// The fewest agents a setup pass may land.
///
/// Below this the team page reads as thin — the failure the whole feature
/// exists to fix. A short roster is topped up from its template rather than
/// shipped, so the floor holds even when a model returns one agent.
pub const MIN_AGENTS: usize = 4;

/// The most agents a setup pass may land. Beyond this a new operator is being
/// handed clutter to tidy rather than a team to work with.
pub const MAX_AGENTS: usize = 6;

/// The longest mandate a card should carry. A model asked for a one-line
/// mandate will occasionally write a paragraph; the roster card has one line
/// for it, so the cap belongs on the data rather than on the CSS.
pub const MAX_DESCRIPTION: usize = 200;

const ECOMMERCE: RosterTemplate = RosterTemplate {
    key: "ecommerce",
    label: "E-commerce",
    keywords: &[
        "ecommerce",
        "e-commerce",
        "online store",
        "shopify",
        "woocommerce",
        "amazon",
        "etsy",
        "dropship",
        "retail",
        "merch",
        "storefront",
        "inventory",
        "fulfilment",
        "fulfillment",
        "dispatch",
        "homeware",
        "apparel",
    ],
    agents: &[
        TemplateAgent {
            name: "Meta Ads",
            role: "Meta Ads Specialist",
            description: "Runs paid campaigns, budgets, and creative testing.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "SEO",
            role: "SEO Specialist",
            description: "Product listings, organic traffic, and search rankings.",
            focus: AgentFocus::Analysis,
        },
        TemplateAgent {
            name: "Logistics",
            role: "Logistics Coordinator",
            description: "Dispatch, tracking, and returns.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "Ops",
            role: "Operations Manager",
            description: "Keeps the rest of the team moving and unblocks them.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "Accounts",
            role: "Accountant",
            description: "Reconciliation, margins, and spend.",
            focus: AgentFocus::Analysis,
        },
    ],
};

const CONTENT: RosterTemplate = RosterTemplate {
    key: "content",
    label: "Content & creator",
    keywords: &[
        "content",
        "creator",
        "influencer",
        "youtube",
        "instagram",
        "tiktok",
        "newsletter",
        "podcast",
        "blog",
        "video",
        "social media",
        "audience",
        "publishing",
    ],
    agents: &[
        TemplateAgent {
            name: "Strategy",
            role: "Content Strategist",
            description: "Decides what to publish, and when.",
            focus: AgentFocus::Analysis,
        },
        TemplateAgent {
            name: "Writer",
            role: "Writer",
            description: "Drafts posts, scripts, and captions.",
            focus: AgentFocus::Writing,
        },
        TemplateAgent {
            name: "Editor",
            role: "Editor",
            description: "Reviews everything before it goes out.",
            focus: AgentFocus::Writing,
        },
        TemplateAgent {
            name: "Social",
            role: "Social Media Manager",
            description: "Schedules posts and works the comments.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "Analyst",
            role: "Analytics Analyst",
            description: "Measures what landed and reports back.",
            focus: AgentFocus::Analysis,
        },
    ],
};

const AGENCY: RosterTemplate = RosterTemplate {
    key: "agency",
    label: "Agency",
    keywords: &[
        "agency",
        "marketing agency",
        "client",
        "clients",
        "campaign",
        "branding",
        "creative studio",
        "design studio",
        "retainer",
    ],
    agents: &[
        TemplateAgent {
            name: "Accounts",
            role: "Account Manager",
            description: "Owns the client relationship and the brief.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "Creative",
            role: "Creative Director",
            description: "Holds the concept and the creative bar.",
            focus: AgentFocus::Writing,
        },
        TemplateAgent {
            name: "Copy",
            role: "Copywriter",
            description: "Writes ads, pages, and campaign copy.",
            focus: AgentFocus::Writing,
        },
        TemplateAgent {
            name: "Media",
            role: "Paid Media Buyer",
            description: "Plans and runs paid acquisition.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "Analyst",
            role: "Analytics Analyst",
            description: "Reports performance back to the client.",
            focus: AgentFocus::Analysis,
        },
    ],
};

const CONSULTING: RosterTemplate = RosterTemplate {
    key: "consulting",
    label: "Consulting",
    keywords: &[
        "consulting",
        "consultancy",
        "advisory",
        "strategy",
        "research firm",
        "diligence",
        "analysis",
        "deck",
        "report writing",
    ],
    agents: &[
        TemplateAgent {
            name: "Engagement",
            role: "Engagement Manager",
            description: "Runs the engagement and keeps it on scope.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "Research",
            role: "Research Analyst",
            description: "Gathers facts, sources, and context.",
            focus: AgentFocus::Research,
        },
        TemplateAgent {
            name: "Modelling",
            role: "Financial Analyst",
            description: "Builds the models and sanity-checks the numbers.",
            focus: AgentFocus::Analysis,
        },
        TemplateAgent {
            name: "Decks",
            role: "Deck Builder",
            description: "Turns findings into something presentable.",
            focus: AgentFocus::Writing,
        },
        TemplateAgent {
            name: "Writer",
            role: "Report Writer",
            description: "Drafts the written deliverable.",
            focus: AgentFocus::Writing,
        },
    ],
};

const SOFTWARE: RosterTemplate = RosterTemplate {
    key: "software",
    label: "Software",
    keywords: &[
        "software",
        "saas",
        "app",
        "product company",
        "startup",
        "platform",
        "api",
        "developer",
        "engineering",
        "b2b",
    ],
    agents: &[
        TemplateAgent {
            name: "Product",
            role: "Product Manager",
            description: "Decides what gets built, and in what order.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "Engineer",
            role: "Software Engineer",
            description: "Builds and ships the product.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "QA",
            role: "QA Engineer",
            description: "Tests changes before they reach anyone.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "Design",
            role: "Product Designer",
            description: "Creates the interface and holds the brand.",
            focus: AgentFocus::Writing,
        },
        TemplateAgent {
            name: "Support",
            role: "Support Specialist",
            description: "Answers customers and closes the loop.",
            focus: AgentFocus::Operations,
        },
    ],
};

/// The roster for a business none of the others describe.
///
/// Deliberately last in [`TEMPLATES`] and reachable only by falling through:
/// it is what a keyword miss lands on, and it is still a real team.
const GENERIC: RosterTemplate = RosterTemplate {
    key: "generic",
    label: "General business",
    keywords: &[],
    agents: &[
        TemplateAgent {
            name: "Ops",
            role: "Operations Lead",
            description: "Keeps work moving and unblocks the team.",
            focus: AgentFocus::Operations,
        },
        TemplateAgent {
            name: "Research",
            role: "Researcher",
            description: "Gathers facts, sources, and context.",
            focus: AgentFocus::Research,
        },
        TemplateAgent {
            name: "Writer",
            role: "Writer",
            description: "Drafts copy, docs, and outbound messages.",
            focus: AgentFocus::Writing,
        },
        TemplateAgent {
            name: "Analyst",
            role: "Analyst",
            description: "Measures performance and reports back.",
            focus: AgentFocus::Analysis,
        },
        TemplateAgent {
            name: "Support",
            role: "Support Specialist",
            description: "Answers customers and closes the loop.",
            focus: AgentFocus::Operations,
        },
    ],
};

/// Every curated roster, most specific first. [`GENERIC`] is last because it
/// matches nothing and is only ever reached as a fallback.
pub const TEMPLATES: &[RosterTemplate] =
    &[ECOMMERCE, CONTENT, AGENCY, CONSULTING, SOFTWARE, GENERIC];

/// The template that best fits these answers, or [`GENERIC`] when none does.
///
/// `industry` is weighted above `automate` because it is the question actually
/// asking what the business *is*; the automation answer only breaks ties.
/// Without the weighting, an e-commerce operator who mentions "social media
/// posts" would be staffed as a content studio — the automation list names
/// tasks, not the business doing them.
pub fn match_template(answers: &SetupAnswers) -> &'static RosterTemplate {
    let industry = answers.industry.to_lowercase();
    let secondary = format!(
        "{} {}",
        answers.team_hint.to_lowercase(),
        answers.automate.to_lowercase()
    );

    let mut best: Option<(&'static RosterTemplate, usize)> = None;
    for template in TEMPLATES {
        let score: usize = template
            .keywords
            .iter()
            .map(|kw| {
                // Three points for naming the business, one for merely
                // mentioning the domain in a task list.
                usize::from(industry.contains(kw)) * 3 + usize::from(secondary.contains(kw))
            })
            .sum();
        if score == 0 {
            continue;
        }
        // Strictly greater, so an earlier (more specific) template holds a tie.
        if best.is_none_or(|(_, seen)| score > seen) {
            best = Some((template, score));
        }
    }
    best.map(|(template, _)| template).unwrap_or(&GENERIC)
}

/// The jobs the operator named, one per item, in the order they wrote them.
///
/// ## Why the host splits this and not the model
///
/// Coverage is only a check if something other than the answer decides what was
/// asked for. If the model both listed the jobs and reported which it had
/// covered, it would be marking its own homework — the list would always match,
/// because both halves come from the same pass. So the host parses the items,
/// numbers them, and verifies the claim against *its* list.
///
/// The split is deliberately dumb: commas, semicolons and newlines, which is how
/// people write a list when a field asks for one. Prose with no separators comes
/// back as a single item, and coverage is then trivially satisfied — that is the
/// honest answer, not a failure. The parsed items are shown back on the review
/// screen, so a bad split is visible to the person who typed it rather than
/// silently shaping a prompt.
///
/// Shared with the console through `tests/fixtures/setup-jobs.json`, which both
/// this module's tests and the frontend's read — two implementations of one rule
/// is exactly how the first version of this feature drifted.
pub fn job_items(automate: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for raw in automate.split([',', ';', '\n', '\r']) {
        let item = raw.trim().trim_end_matches('.').trim();
        if item.is_empty() {
            continue;
        }
        // De-duplicated case-insensitively: someone who writes the same job
        // twice should not make it impossible to cover their list.
        if items
            .iter()
            .any(|seen: &String| seen.eq_ignore_ascii_case(item))
        {
            continue;
        }
        items.push(item.to_string());
        if items.len() >= MAX_JOBS {
            break;
        }
    }
    items
}

/// The most jobs a checklist may carry.
///
/// Not a limit on what someone may want — a limit on what one prompt can be
/// asked to cover with six teammates. Past this the list stops being a checklist
/// and becomes a backlog, and a roster that "covers" forty items covers none of
/// them.
pub const MAX_JOBS: usize = 12;

/// The positions in `jobs` that no agent claimed, in the order they were
/// written.
///
/// Indices are the model's claim and are bounds-checked by construction rather
/// than trusted: this walks the host's own list, so an out-of-range claim covers
/// nothing because it names nothing.
///
/// Positions rather than strings because the re-ask has to speak the *same*
/// numbering as the first ask. Renumbering the gaps from zero — which the first
/// version of the re-ask did — makes the second answer's `covers` refer to a
/// different list than the first's, and the two silently disagree.
pub fn uncovered_indices(jobs: &[String], claimed: &[usize]) -> Vec<usize> {
    (0..jobs.len()).filter(|i| !claimed.contains(i)).collect()
}

/// The items in `jobs` that no agent claimed, in the order they were written.
pub fn uncovered_jobs(jobs: &[String], claimed: &[usize]) -> Vec<String> {
    uncovered_indices(jobs, claimed)
        .into_iter()
        .filter_map(|i| jobs.get(i).cloned())
        .collect()
}

/// Whether every role on this roster came from the reference team it was shown.
///
/// The degenerate case the reference team invites. `match_template`'s roster goes
/// into the prompt as a quality bar for naming and phrasing, and a model that
/// takes it as a menu can hand the whole thing back unchanged. Nothing about the
/// *shape* of that answer is wrong — bounds pass, roles are unique, mandates fit
/// — so validation admits it, and the operator is then told "built from what you
/// told us" about a roster nobody designed.
///
/// This is the one claim worth policing deterministically. It does **not** police
/// style: a designed line-up that borrows a sentence or two is still a designed
/// line-up, and the prompt asks for the operator's own words without the host
/// enforcing prose. What it refuses is calling a copy an original.
///
/// Roles are compared by slug, so a re-spacing or a case change does not read as
/// authorship.
pub fn is_entirely_reference_team(agents: &[ProposedAgent], template: &RosterTemplate) -> bool {
    if agents.is_empty() {
        return false;
    }
    let reference: Vec<String> = template.agents.iter().map(|a| role_slug(a.role)).collect();
    agents
        .iter()
        .all(|agent| reference.contains(&role_slug(&agent.role)))
}

/// A roster a setup pass is offering the operator, and where it came from.
///
/// Carries its provenance because the console says so out loud — decision D2 is
/// that everything setup builds is presented as a starting point, and "we picked
/// the e-commerce team" is a more honest thing to show than a roster that
/// appears from nowhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterProposal {
    pub agents: Vec<ProposedAgent>,
    /// The [`RosterTemplate::key`] whose reference team framed this proposal.
    /// Reported for either source: it is what the model was shown as a quality
    /// bar, and what a failure fell back to.
    pub template_key: &'static str,
    /// Who wrote this team.
    pub source: RosterSource,
    /// The jobs the operator named, as [`job_items`] split them. Echoed back on
    /// the review screen so the list a roster was judged against is the list
    /// they can see.
    pub jobs: Vec<String>,
    /// The jobs no teammate on this roster owns.
    ///
    /// Only ever non-empty on the [`Model`](RosterSource::Model) path: coverage
    /// is a claim the design pass makes and the host checks, and a curated team
    /// makes no claim about a list it never read. A fallback roster reports its
    /// provenance instead, which is the honest thing to say about it.
    pub uncovered: Vec<String>,
    /// Why this is the curated team, when it is. `None` on the model path.
    ///
    /// The review screen said "we couldn't reach a model" for every fallback,
    /// which is false in the two cases where a model answered and its answer was
    /// unusable. See [`FallbackReason`].
    pub reason: Option<FallbackReason>,
}

/// Who wrote a proposed roster.
///
/// Replaces an earlier `generated: bool`, which was accurate and read as a lie.
/// `generated = true` meant "a model answered the call" — but with the whole
/// roster still assembled from canned strings it was taken to mean "a model
/// wrote these words", which it did not. Naming the source makes the difference
/// unmissable, and lets the console say which one an operator is looking at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosterSource {
    /// A model designed this team from the operator's own answers.
    Model,
    /// The curated team for this kind of business, shipped whole because no
    /// model was reachable, its answer could not be read, or what it returned
    /// was too thin to be a company. Never blended with a model's answer —
    /// see [`validate_roster`].
    Fallback,
}

/// Why a roster fell back to the curated team.
///
/// ## The copy was telling operators something false
///
/// The review screen said "we couldn't reach a model to tailor it" for *every*
/// fallback, because [`RosterSource::Fallback`] was the only thing it had to go
/// on. That is true when no credential is wired and false in the two cases where
/// a model answered and its answer was unusable — the operator is then told the
/// host could not reach something it reached fine.
///
/// It matters because the **action differs**. No model means "add a key". An
/// unusable answer means "you told us very little; go back and say more". A
/// single sentence covering both can only be vague enough to be useless.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// No credential was reachable, so no design pass ran at all.
    NoModel,
    /// A model answered and the answer could not be used: unreadable, too thin
    /// to be a company, or the reference team handed back unchanged. Almost
    /// always means the operator's answers were too sparse to design from.
    NotDesignable,
}

impl FallbackReason {
    /// The wire spelling the console reads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoModel => "no_model",
            Self::NotDesignable => "not_designable",
        }
    }
}

impl RosterSource {
    /// The wire spelling the console reads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Fallback => "fallback",
        }
    }
}

/// The proposal for these answers with no model involved: the matched template,
/// validated.
///
/// This is both the fast path (no inference credential wired) and the floor
/// every other path falls back to, which is why it lives here rather than
/// beside the pass that polishes it.
pub fn template_proposal(answers: &SetupAnswers, reason: FallbackReason) -> RosterProposal {
    let template = match_template(answers);
    RosterProposal {
        agents: validate_roster(template.proposed()),
        template_key: template.key,
        source: RosterSource::Fallback,
        reason: Some(reason),
        jobs: job_items(&answers.automate),
        // A curated team was chosen by keyword, not designed against this list,
        // so it claims nothing about it. Saying "all of it is uncovered" would
        // be as misleading as saying none of it is.
        uncovered: Vec::new(),
    }
}

/// Turns a proposed roster into a company the runtime can register.
///
/// The wizard's apply needs a [`CompanyManifest`], not a template directory:
/// [`register`](crate::desktop::register) has always taken one, and
/// `first_run_manifest` builds a preset the same way — parse a base, then set
/// `agents`. Generating a company is therefore this function plus that call,
/// rather than a new subsystem.
///
/// ## What it decides, and what it refuses to
///
/// * **`[policy].mode` comes from [`PROVISIONED_POLICY_MODE`], not from a
///   literal here.** A company this flow creates must be indistinguishable from
///   one `POST /api/v1/companies` provisions; hard-coding a different tier would
///   fork the meaning of "a new company's default" across two call sites, and
///   the next person to move it would move only one.
/// * **The admin address is written into `[users].admins`.** Without it a laptop
///   operator who chose email sign-in completes setup and can then sign in as
///   nobody — no shipped template invites anyone, so the address they typed is
///   the only thing standing between them and a locked-out host.
/// * **It does not invent desks, workflows, schedules or budgets.** The roster
///   is what the operator reviewed; everything else stays at its manifest
///   default, where a later edit is an ordinary change rather than an
///   unpicking of something setup assumed.
///
/// Agent ids are derived from roles rather than minted from a counter, so the
/// same reviewed roster always produces the same ids — and de-duplicated with a
/// numeric suffix, because `validate` rejects a repeat and two roles can slug
/// alike.
pub fn manifest_from_setup(
    answers: &SetupAnswers,
    agents: &[ProposedAgent],
    admin_email: Option<&str>,
) -> crate::company::CompanyManifest {
    let name = company_name(answers);
    let mut manifest: crate::company::CompanyManifest =
        toml::from_str("[company]\nname = \"placeholder\"\n")
            .expect("a name-only manifest is always parseable");

    manifest.company.name = name;
    manifest.company.output = non_empty(&answers.automate);
    manifest.company.human_role = Some(HUMAN_ROLE.to_string());
    manifest.policy.mode = crate::company::PROVISIONED_POLICY_MODE.to_string();

    if let Some(email) = admin_email.map(str::trim).filter(|e| !e.is_empty()) {
        manifest.users.admins = vec![email.to_string()];
    }

    // Parsed rather than constructed field-by-field: `Agent` carries a dozen
    // optional fields with serde defaults, and enumerating them here would mean
    // this function silently missing whichever one is added next.
    let blank: crate::company::Agent =
        toml::from_str("id = \"placeholder\"\nrole = \"placeholder\"\n")
            .expect("an id+role agent is always parseable");

    let mut seen: Vec<String> = Vec::new();
    manifest.agents = agents
        .iter()
        .map(|agent| {
            let mut built = blank.clone();
            built.id = unique_agent_id(&agent.role, &mut seen);
            built.role = agent.role.trim().to_string();
            built.description = non_empty(&agent.description);
            // Asked for explicitly, exactly as `globals/agents/*.toml` do. An
            // agent that requests nothing inherits the company belt whole —
            // which here is the globals default `["*", "media", "composio"]`,
            // so every teammate a first-run operator created held real-money
            // media and per-tenant Composio credentials. Intersected with
            // `[tools].allow`, so this can only ever narrow.
            built.tools = tools_for_focus(agent.focus);
            built
        })
        .collect();

    manifest
}

/// What the human keeps, stated the same way for every company this flow builds.
///
/// `[company].human_role` is a required-feeling field an operator has not been
/// asked about, and inventing a per-company answer from three sentences would be
/// guessing at the one thing the product says is theirs. A constant is honest
/// and editable.
const HUMAN_ROLE: &str = "Direction, and the calls that matter";

/// The company's display name, drawn from what they said they do.
///
/// Deliberately not a fifth question. A name is the easiest thing in the world
/// to change later and the most annoying thing to be asked for before you have
/// seen anything — so the first clause of their own sentence becomes the name,
/// and the Settings page renames it in one field.
fn company_name(answers: &SetupAnswers) -> String {
    let raw = answers.industry.trim();
    if raw.is_empty() {
        return "My Company".to_string();
    }
    // The first clause: people write "E-commerce — I sell homeware online", and
    // the half before the dash is the name they would have typed.
    //
    // A *spaced* hyphen is a clause break; a bare one is part of a word, and
    // splitting on it turned "E-commerce" into "E". So the spaced forms are
    // folded to an em dash first and the bare hyphen is never a separator.
    let normalised = raw.replace(" - ", "—").replace(" – ", "—");
    let head = normalised
        .split(['—', ',', '.', ':', ';', '\n'])
        .map(str::trim)
        .find(|part| !part.is_empty())
        .unwrap_or(raw)
        .to_string();
    let head = head.as_str();
    let trimmed: String = head.chars().take(60).collect();
    if trimmed.trim().is_empty() {
        "My Company".to_string()
    } else {
        trimmed.trim().to_string()
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A snake_case id for this role that no earlier row has taken.
///
/// `validate` rejects a duplicate id and a non-snake_case one, so both are
/// handled here rather than surfaced to an operator who typed nothing wrong.
fn unique_agent_id(role: &str, seen: &mut Vec<String>) -> String {
    let base = snake_id(role);
    if !seen.contains(&base) {
        seen.push(base.clone());
        return base;
    }
    for n in 2..1000 {
        let candidate = format!("{base}_{n}");
        if !seen.contains(&candidate) {
            seen.push(candidate.clone());
            return candidate;
        }
    }
    base
}

/// Lowercase letters, digits and underscores, starting with a letter — the
/// shape `is_snake_case` demands.
fn snake_id(role: &str) -> String {
    let mut id = String::with_capacity(role.len());
    let mut pending = false;
    for ch in role.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending && !id.is_empty() {
                id.push('_');
            }
            pending = false;
            id.push(ch.to_ascii_lowercase());
        } else {
            pending = true;
        }
    }
    // Must start with a lowercase letter: a role like "3D Artist" would
    // otherwise produce an id the validator refuses.
    match id.chars().next() {
        Some(c) if c.is_ascii_lowercase() => id,
        _ if id.is_empty() => "teammate".to_string(),
        _ => format!("a_{id}"),
    }
}

/// A role's identity for de-duplication: lowercase alphanumerics, everything
/// else collapsed to a single `-`.
fn role_slug(role: &str) -> String {
    let mut slug = String::with_capacity(role.len());
    let mut pending_dash = false;
    for ch in role.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    slug
}

/// Truncates a mandate to [`MAX_DESCRIPTION`] on a word boundary where one is
/// near, so a long answer reads as a sentence rather than a severed word.
fn clamp_description(description: &str) -> String {
    let trimmed = description.trim();
    if trimmed.chars().count() <= MAX_DESCRIPTION {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(MAX_DESCRIPTION).collect();
    let boundary = cut.rfind(' ').unwrap_or(cut.len());
    // Only honour the boundary if it keeps most of the text; a string with one
    // space near the start would otherwise be cut to almost nothing.
    let kept = if boundary > MAX_DESCRIPTION / 2 {
        &cut[..boundary]
    } else {
        cut.as_str()
    };
    format!("{}…", kept.trim_end_matches([' ', ',', ';', '.']))
}

/// Brings a proposed roster inside the rules every roster obeys, whoever
/// produced it.
///
/// Applied to **model output and template output alike**, so there is one
/// definition of a well-formed roster rather than one per producer:
///
/// * fields trimmed; a blank name falls back to the role;
/// * an entry with no role is dropped — it names nobody;
/// * mandates clamped to [`MAX_DESCRIPTION`];
/// * duplicate roles collapsed (first wins), so a model that repeats itself
///   cannot land two teammates who share one job;
/// * truncated to [`MAX_AGENTS`].
///
/// ## It does not top a short roster up, and used to
///
/// An earlier version padded anything under [`MIN_AGENTS`] with agents from the
/// matched template. It produced exactly the outcome it was meant to prevent: a
/// yoga studio asked for bookings and retention, the pass returned three agents,
/// and the fourth teammate the operator was shown was a **Content Strategist**
/// — from a template they had never seen, for work they had not mentioned. The
/// padding was invisible in the result, so the roster read as though a model had
/// chosen it.
///
/// Three relevant teammates beat four with one stranger in them. A roster too
/// thin to be a company is now the *caller's* decision, made by comparing
/// against [`MIN_AGENTS`] and falling back to the curated team **whole** — so an
/// operator is always looking at one authored team or the other, never a blend
/// of both. See [`crate::harness::roster_build`].
pub fn validate_roster(proposed: Vec<ProposedAgent>) -> Vec<ProposedAgent> {
    let mut seen: Vec<String> = Vec::new();
    let mut roster: Vec<ProposedAgent> = Vec::new();

    let push = |agent: ProposedAgent, roster: &mut Vec<ProposedAgent>, seen: &mut Vec<String>| {
        let role = agent.role.trim();
        if role.is_empty() || roster.len() >= MAX_AGENTS {
            return;
        }
        let slug = role_slug(role);
        if slug.is_empty() || seen.contains(&slug) {
            return;
        }
        let name = agent.name.trim();
        seen.push(slug);
        roster.push(ProposedAgent {
            name: if name.is_empty() {
                role.to_string()
            } else {
                name.to_string()
            },
            role: role.to_string(),
            description: clamp_description(&agent.description),
            // Carried through untouched. Validation bounds the *shape* of a
            // roster; the belt is decided by `tools_for_focus`, and an unknown
            // focus has already become `None` at the wire.
            focus: agent.focus,
        });
    };

    for agent in proposed {
        push(agent, &mut roster, &mut seen);
    }
    roster
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers(industry: &str, automate: &str) -> SetupAnswers {
        SetupAnswers {
            industry: industry.to_string(),
            team_hint: String::new(),
            automate: automate.to_string(),
        }
    }

    fn agent(role: &str) -> ProposedAgent {
        ProposedAgent {
            name: role.to_string(),
            role: role.to_string(),
            description: "does the thing".to_string(),
            focus: None,
        }
    }

    /// The spec's worked example: "I sell homeware online" must staff the
    /// e-commerce team, mandate-for-mandate.
    #[test]
    fn the_worked_example_lands_the_ecommerce_roster() {
        let picked = match_template(&answers(
            "E-commerce — I sell homeware online",
            "Social media posts, Meta ads, generating my reports, order dispatch",
        ));
        assert_eq!(picked.key, "ecommerce");
        let roles: Vec<&str> = picked.agents.iter().map(|a| a.role).collect();
        assert!(roles.contains(&"Logistics Coordinator"), "{roles:?}");
        assert!(roles.contains(&"Meta Ads Specialist"), "{roles:?}");
    }

    /// The weighting that keeps the automation list from overruling the
    /// business. An e-commerce operator naming social posts is still running a
    /// shop, and staffing them as a content studio would leave nobody on
    /// dispatch.
    #[test]
    fn the_industry_answer_outweighs_the_automation_list() {
        let picked = match_template(&answers(
            "online store selling homeware",
            "instagram, tiktok, youtube, podcast, newsletter, blog",
        ));
        assert_eq!(picked.key, "ecommerce");
    }

    /// The automation answer still decides when the industry says nothing
    /// recognisable — it is the tiebreak, not dead weight.
    #[test]
    fn the_automation_answer_breaks_a_tie() {
        let picked = match_template(&answers("just me", "scheduling my youtube uploads"));
        assert_eq!(picked.key, "content");
    }

    /// A miss must land a real team, not nothing. This is decision D3's cheap
    /// half: the never-strand fallback is a curated roster.
    #[test]
    fn an_unrecognised_business_still_gets_a_real_team() {
        let picked = match_template(&answers("zzzz qqqq", ""));
        assert_eq!(picked.key, "generic");
        assert!(picked.agents.len() >= MIN_AGENTS);
    }

    /// Every curated roster must itself satisfy the rules it is the fallback
    /// for. A template that could not pass validation would be a floor that
    /// does not hold.
    #[test]
    fn every_template_is_within_its_own_bounds() {
        for template in TEMPLATES {
            let count = template.agents.len();
            assert!(
                (MIN_AGENTS..=MAX_AGENTS).contains(&count),
                "{} has {count} agents",
                template.key
            );
            let validated = validate_roster(template.proposed());
            assert_eq!(
                validated.len(),
                count,
                "{} lost agents to validation",
                template.key
            );
            for a in template.agents {
                assert!(
                    !a.role.trim().is_empty(),
                    "{} has a blank role",
                    template.key
                );
                assert!(
                    a.description.chars().count() <= MAX_DESCRIPTION,
                    "{} has an over-long mandate",
                    template.key
                );
            }
        }
    }

    /// Template keys are how a proposal reports which roster it came from, so
    /// two templates sharing one would make that report ambiguous.
    #[test]
    fn template_keys_are_unique() {
        let mut keys: Vec<&str> = TEMPLATES.iter().map(|t| t.key).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "duplicate template key");
    }

    #[test]
    fn an_over_long_roster_is_truncated() {
        let long: Vec<ProposedAgent> = (0..12).map(|i| agent(&format!("Role {i}"))).collect();
        assert_eq!(validate_roster(long).len(), MAX_AGENTS);
    }

    /// **No padding.** A short roster comes back short, so nothing an operator is
    /// shown was quietly borrowed from a template they never saw.
    ///
    /// The regression this guards is concrete: a yoga studio's pass returned
    /// three agents, validation padded it to four from the `content` template,
    /// and the fourth teammate on screen was a Content Strategist — rendered
    /// identically to the three the operator had actually asked for. Deciding
    /// what to do about a thin roster belongs to the caller, which falls back to
    /// the curated team **whole**.
    #[test]
    fn a_short_roster_is_left_short_rather_than_padded() {
        let roster = validate_roster(vec![agent("Meta Ads Specialist")]);
        assert_eq!(roster.len(), 1, "validation must not invent teammates");
        assert_eq!(roster[0].role, "Meta Ads Specialist");
    }

    /// Two teammates sharing one job is the failure the operator would have to
    /// clean up by hand, so near-miss spellings collapse too.
    #[test]
    fn duplicate_roles_collapse_however_they_are_spelled() {
        let roster = validate_roster(vec![
            agent("SEO Specialist"),
            agent("seo  specialist"),
            agent("SEO-Specialist"),
        ]);
        let seo = roster
            .iter()
            .filter(|a| role_slug(&a.role) == "seo-specialist")
            .count();
        assert_eq!(seo, 1, "{roster:?}");
    }

    #[test]
    fn a_roleless_entry_is_dropped_and_a_blank_name_falls_back_to_the_role() {
        let roster = validate_roster(vec![
            ProposedAgent {
                name: "Ghost".into(),
                role: "   ".into(),
                description: String::new(),
                focus: None,
            },
            ProposedAgent {
                name: "  ".into(),
                role: "Data Analyst".into(),
                description: String::new(),
                focus: None,
            },
        ]);
        assert!(roster.iter().all(|a| !a.role.trim().is_empty()));
        let analyst = roster.iter().find(|a| a.role == "Data Analyst").unwrap();
        assert_eq!(analyst.name, "Data Analyst");
    }

    /// A model asked for one line occasionally writes a paragraph. The card has
    /// one line for it, so the cap is on the data.
    #[test]
    fn an_over_long_mandate_is_clamped() {
        let essay = "word ".repeat(200);
        let roster = validate_roster(vec![ProposedAgent {
            name: "A".into(),
            role: "Analyst".into(),
            description: essay,
            focus: None,
        }]);
        let clamped = &roster[0].description;
        assert!(clamped.chars().count() <= MAX_DESCRIPTION + 1, "{clamped}");
        assert!(clamped.ends_with('…'), "{clamped}");
    }

    /// Validation of nothing is nothing. The floor is the caller's business now,
    /// and `template_proposal` is where an operator with no usable model still
    /// gets a real team.
    #[test]
    fn validation_of_an_empty_roster_stays_empty() {
        assert!(validate_roster(Vec::new()).is_empty());
    }

    /// The honest fallback: a full curated team, labelled as such, for the
    /// offline path and every failure path.
    #[test]
    fn the_fallback_is_a_whole_curated_team_and_says_so() {
        let proposal = template_proposal(
            &answers("I sell homeware online", ""),
            FallbackReason::NoModel,
        );
        assert_eq!(proposal.template_key, "ecommerce");
        assert_eq!(proposal.source, RosterSource::Fallback);
        assert_eq!(proposal.source.as_str(), "fallback");
        assert!(
            proposal.agents.len() >= MIN_AGENTS,
            "a fallback must be a workable team, got {}",
            proposal.agents.len()
        );
        // Whole, not blended: every row is the template's own.
        let curated: Vec<&str> = ECOMMERCE.agents.iter().map(|a| a.role).collect();
        for a in &proposal.agents {
            assert!(
                curated.contains(&a.role.as_str()),
                "{} is not curated",
                a.role
            );
        }
    }

    // ---------------------------------------------------------------------
    // Synthesising a company from the answers
    // ---------------------------------------------------------------------

    fn proposed(role: &str) -> ProposedAgent {
        ProposedAgent {
            name: role.split_whitespace().next().unwrap_or(role).to_string(),
            role: role.to_string(),
            description: format!("Owns {}.", role.to_lowercase()),
            focus: None,
        }
    }

    /// The whole point of the synthesis: what comes out must be a company the
    /// runtime will accept. `validate` is what `opencompany check` runs, so an
    /// empty problem list is the same bar a hand-written manifest clears.
    #[test]
    fn a_synthesised_company_passes_validation() {
        let answers = answers("E-commerce — I sell homeware online", "Meta ads, dispatch");
        let roster = vec![
            proposed("Meta Ads Specialist"),
            proposed("Order Dispatch Coordinator"),
            proposed("Accountant"),
            proposed("Operations Lead"),
        ];
        let manifest = manifest_from_setup(&answers, &roster, Some("ada@example.com"));
        assert_eq!(manifest.validate(), Vec::<String>::new());
        assert_eq!(manifest.agents.len(), 4);
    }

    /// The dead end this flow exists to close: no shipped template invites
    /// anybody, so an operator who picks email sign-in and is not written into
    /// `[users].admins` completes setup and can then sign in as nobody.
    #[test]
    fn the_operator_is_invited_as_an_admin() {
        let manifest = manifest_from_setup(
            &answers("a shop", ""),
            &[proposed("Accountant")],
            Some("  ada@example.com  "),
        );
        assert_eq!(manifest.users.admins, vec!["ada@example.com".to_string()]);
    }

    /// A host that needs no sign-in supplies no address, and inviting `""`
    /// would put an unusable row in the admin list.
    #[test]
    fn no_address_invites_nobody() {
        for email in [None, Some(""), Some("   ")] {
            let manifest = manifest_from_setup(&answers("a shop", ""), &[proposed("Ops")], email);
            assert!(manifest.users.admins.is_empty(), "{email:?}");
        }
    }

    /// Setup-created and provision-created companies must be indistinguishable.
    /// Reading the constant rather than a literal is what keeps them that way
    /// when the product next moves the default (#605).
    #[test]
    fn the_policy_tier_is_the_provisioned_default_not_a_literal() {
        let manifest = manifest_from_setup(&answers("a shop", ""), &[proposed("Ops")], None);
        assert_eq!(
            manifest.policy.mode,
            crate::company::PROVISIONED_POLICY_MODE
        );
    }

    /// `validate` rejects duplicate ids, and two roles can slug alike — so the
    /// de-duplication has to happen here rather than surface to an operator who
    /// typed nothing wrong.
    #[test]
    fn roles_that_slug_alike_still_get_distinct_ids() {
        let manifest = manifest_from_setup(
            &answers("a shop", ""),
            &[
                proposed("Ops Lead"),
                proposed("ops  lead"),
                proposed("OPS-LEAD"),
            ],
            None,
        );
        let ids: Vec<&str> = manifest.agents.iter().map(|a| a.id.as_str()).collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        assert_eq!(manifest.validate(), Vec::<String>::new());
    }

    /// A role that starts with a digit slugs to something `is_snake_case`
    /// refuses, and the operator never sees why. Handled here instead.
    #[test]
    fn a_role_starting_with_a_digit_still_yields_a_valid_id() {
        let manifest =
            manifest_from_setup(&answers("a studio", ""), &[proposed("3D Artist")], None);
        assert_eq!(manifest.validate(), Vec::<String>::new());
        assert!(
            manifest.agents[0]
                .id
                .starts_with(|c: char| c.is_ascii_lowercase()),
            "{}",
            manifest.agents[0].id
        );
    }

    /// The name is taken from the first clause of their own sentence rather
    /// than asked for — a name is trivial to change later and tedious to be
    /// asked for before you have seen anything.
    #[test]
    fn the_company_is_named_from_the_first_clause() {
        for (typed, expected) in [
            ("E-commerce — I sell homeware online", "E-commerce"),
            // A spaced hyphen is the same clause break, typed by someone whose
            // keyboard has no em dash.
            ("E-commerce - I sell homeware online", "E-commerce"),
            (
                "A yoga studio in Pune, drop-in classes",
                "A yoga studio in Pune",
            ),
            // No separator at all: the whole sentence is the name.
            ("Homeware shop", "Homeware shop"),
        ] {
            let manifest = manifest_from_setup(&answers(typed, ""), &[proposed("Ops")], None);
            assert_eq!(manifest.company.name, expected, "typed: {typed}");
        }
    }

    /// The hyphen regression, kept as its own case because it is the one a
    /// reader would not predict: "E-commerce" must never become "E".
    #[test]
    fn a_hyphen_inside_a_word_does_not_split_the_name() {
        let manifest = manifest_from_setup(
            &answers("e-commerce and drop-shipping", ""),
            &[proposed("Ops")],
            None,
        );
        assert_eq!(manifest.company.name, "e-commerce and drop-shipping");
    }

    /// Someone who typed nothing still gets a valid, named company.
    #[test]
    fn an_unnamed_business_still_yields_a_valid_company() {
        let manifest = manifest_from_setup(&SetupAnswers::default(), &[proposed("Ops")], None);
        assert!(!manifest.company.name.trim().is_empty());
        assert_eq!(manifest.validate(), Vec::<String>::new());
    }

    /// Setup builds a roster and nothing else. Desks, workflows, schedules and
    /// budgets stay at their defaults, so a later edit is an ordinary change
    /// rather than an unpicking of something setup assumed.
    #[test]
    fn synthesis_invents_nothing_beyond_the_roster() {
        let manifest = manifest_from_setup(
            &answers("a shop", "everything"),
            &[proposed("Ops"), proposed("Accountant")],
            None,
        );
        assert!(manifest.group_chats.is_empty(), "no desks were asked for");
        assert!(manifest.schedules.is_empty(), "no schedule was asked for");
    }

    /// The answers ride on the company record, so they must survive the round
    /// trip the record makes through its store.
    #[test]
    fn answers_round_trip_through_serde() {
        let answers = SetupAnswers {
            industry: "E-commerce".into(),
            team_hint: "plus customer support".into(),
            automate: "Meta ads, order dispatch".into(),
        };
        let json = serde_json::to_string(&answers).expect("serialize");
        assert_eq!(
            serde_json::from_str::<SetupAnswers>(&json).expect("deserialize"),
            answers
        );
        // And a record written before setup existed still loads.
        assert_eq!(
            serde_json::from_str::<SetupAnswers>("{}").expect("empty"),
            SetupAnswers::default()
        );
    }

    // ---------------------------------------------------------------------
    // Focus, and the belt it decides
    // ---------------------------------------------------------------------

    /// The control, quantified over the **whole** vocabulary rather than the
    /// four focuses a reader happened to remember.
    ///
    /// `media` spends real money, `composio` reaches per-tenant credentials,
    /// `search` bills per call, `repo` reaches bound source, and `shell` is
    /// arbitrary execution. None of them may be reachable from a job shape a
    /// model chose after reading free text a stranger typed — those stay
    /// company-level grants an operator makes on purpose. A fifth focus added
    /// later fails here unless it obeys the same rule.
    #[test]
    fn no_focus_ever_confers_money_credentials_or_a_shell() {
        const FORBIDDEN: [&str; 5] = ["media", "composio", "search", "repo", "shell"];
        for focus in AgentFocus::ALL {
            for grant in focus.tools() {
                let namespace = grant.split(['.', '_', ':']).next().unwrap_or(&grant);
                assert!(
                    !FORBIDDEN.contains(&namespace),
                    "{} grants `{grant}`",
                    focus.as_str()
                );
                // A bare `*` would confer everything the wildcard covers, which
                // is the inherit-the-lot behaviour focus exists to end.
                assert_ne!(grant, "*", "{} grants the catch-all", focus.as_str());
            }
        }
    }

    /// The bug this whole seam exists to close.
    ///
    /// `manifest_from_setup` parses a name-only base, so `[tools]` takes the
    /// globals default `["*", "media", "composio"]` — and an agent that asks for
    /// nothing inherits that belt whole. Every teammate a first-run operator
    /// created therefore held real-money media and per-tenant Composio
    /// credentials for a company described in three sentences.
    #[test]
    fn a_designed_teammate_asks_for_a_belt_instead_of_inheriting_the_company_one() {
        let roster = vec![ProposedAgent {
            name: "Research".into(),
            role: "Research Analyst".into(),
            description: "Finds things out.".into(),
            focus: Some(AgentFocus::Research),
        }];
        let manifest = manifest_from_setup(&answers("a shop", ""), &roster, None);
        let asked = &manifest.agents[0].tools;

        assert!(!asked.is_empty(), "an empty list inherits the company belt");
        assert!(!asked.iter().any(|t| t == "media" || t == "composio"));
        assert_eq!(manifest.validate(), Vec::<String>::new());

        // The company belt itself is untouched: narrowing happens per teammate,
        // so an operator who later widens `[tools].allow` is not fighting a
        // decision setup made for them.
        assert!(manifest.tools.allow.iter().any(|g| g == "*"));
    }

    /// A model that invents `"marketing"` costs that teammate its narrowing —
    /// never the operator their roster. `None` is the pre-focus behaviour: worse,
    /// but working.
    #[test]
    fn an_unreadable_focus_degrades_to_inheriting_rather_than_failing() {
        for invented in ["marketing", "", "  ", "RESEARCH!"] {
            assert_eq!(AgentFocus::from_wire(invented), None, "{invented:?}");
        }
        // Fail CLOSED: never an empty list, because empty means "inherit the
        // company belt" — which for a setup-built company is
        // `["*", "media", "composio"]`. An unrecognised value must not buy more
        // authority than a recognised one.
        let unknown = tools_for_focus(None);
        assert!(!unknown.is_empty(), "an empty belt inherits everything");
        assert_eq!(unknown, AgentFocus::Writing.tools());
        // And it must not take the surrounding roster down at the wire.
        let wire = r#"{"name":"A","role":"Analyst","description":"d","focus":"marketing"}"#;
        let parsed: ProposedAgent = serde_json::from_str(wire).expect("unknown focus must parse");
        assert_eq!(parsed.focus, None);
        assert_eq!(parsed.role, "Analyst");
    }

    /// The fallback team is scoped exactly as a designed one is. An operator
    /// with no credential must not end up with the *wider* company — which is
    /// what would happen if only the model path carried a focus.
    #[test]
    fn the_curated_fallback_is_scoped_too() {
        let proposal = template_proposal(
            &answers("I sell homeware online", ""),
            FallbackReason::NoModel,
        );
        assert!(proposal.agents.iter().all(|a| a.focus.is_some()));
        let manifest = manifest_from_setup(
            &answers("I sell homeware online", ""),
            &proposal.agents,
            None,
        );
        for agent in &manifest.agents {
            assert!(!agent.tools.is_empty(), "{} inherits the lot", agent.id);
        }
        assert_eq!(manifest.validate(), Vec::<String>::new());
    }

    /// Focus survives the round trip through the review screen, which is the
    /// only reason the belt an operator approves is the belt they get.
    #[test]
    fn focus_round_trips_through_serde() {
        for focus in AgentFocus::ALL {
            let agent = ProposedAgent {
                name: "A".into(),
                role: "Analyst".into(),
                description: "d".into(),
                focus: Some(focus),
            };
            let json = serde_json::to_string(&agent).expect("serialize");
            assert!(json.contains(focus.as_str()), "{json}");
            let back: ProposedAgent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back.focus, Some(focus));
        }
        // A roster written before focus existed still loads.
        let old = r#"{"name":"A","role":"Analyst","description":"d"}"#;
        assert_eq!(
            serde_json::from_str::<ProposedAgent>(old)
                .expect("legacy")
                .focus,
            None
        );
    }

    // ---------------------------------------------------------------------
    // The job checklist coverage is judged against
    // ---------------------------------------------------------------------

    /// The splitting rule, from the fixture the console's test reads too.
    ///
    /// The fixture is the whole mitigation for having two implementations of one
    /// rule: the console echoes the items live while someone types, and the host
    /// numbers them for the prompt. The first version of this feature shipped a
    /// hand-copied keyword list in the browser and it drifted within a week.
    #[test]
    fn job_items_matches_the_shared_fixture() {
        #[derive(serde::Deserialize)]
        struct Case {
            why: String,
            input: String,
            items: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct Fixture {
            #[serde(rename = "maxJobs")]
            max_jobs: usize,
            cases: Vec<Case>,
        }

        let raw = include_str!("../../tests/fixtures/setup-jobs.json");
        let fixture: Fixture = serde_json::from_str(raw).expect("fixture parses");
        assert_eq!(
            fixture.max_jobs, MAX_JOBS,
            "the fixture and the host disagree about the cap"
        );
        assert!(
            !fixture.cases.is_empty(),
            "an empty fixture asserts nothing"
        );
        for case in fixture.cases {
            assert_eq!(job_items(&case.input), case.items, "{}", case.why);
        }
    }

    /// Coverage is set maths over the host's list, not a sentence from the
    /// model. An index that names nothing covers nothing.
    #[test]
    fn an_out_of_range_claim_covers_nothing() {
        let jobs = job_items("ads, dispatch, invoices");
        assert_eq!(
            uncovered_jobs(&jobs, &[0, 99]),
            vec!["dispatch", "invoices"]
        );
        assert!(uncovered_jobs(&jobs, &[0, 1, 2]).is_empty());
        assert_eq!(uncovered_jobs(&jobs, &[]), jobs);
    }

    /// A curated team was chosen by keyword and never read the list, so it
    /// reports its provenance rather than a coverage claim it cannot make.
    #[test]
    fn the_fallback_echoes_the_jobs_but_claims_no_coverage() {
        let proposal = template_proposal(
            &answers("I sell homeware online", "Meta ads, order dispatch"),
            FallbackReason::NoModel,
        );
        assert_eq!(proposal.jobs, vec!["Meta ads", "order dispatch"]);
        assert!(
            proposal.uncovered.is_empty(),
            "a fallback must not claim a gap it never looked for"
        );
    }

    // ---------------------------------------------------------------------
    // Refusing to call a copy an original
    // ---------------------------------------------------------------------

    /// The degenerate answer the reference team invites: hand the whole thing
    /// back. Nothing about its *shape* is wrong, so validation admits it — and
    /// the operator would then be told "built from what you told us" about a
    /// roster nobody designed.
    #[test]
    fn a_roster_that_is_only_the_reference_team_is_recognised() {
        assert!(is_entirely_reference_team(
            &ECOMMERCE.proposed(),
            &ECOMMERCE
        ));
        // Re-spacing and re-casing are not authorship.
        let restyled: Vec<ProposedAgent> = ECOMMERCE
            .agents
            .iter()
            .map(|a| ProposedAgent {
                name: a.name.to_string(),
                role: a.role.to_uppercase().replace(' ', "  "),
                description: a.description.to_string(),
                focus: Some(a.focus),
            })
            .collect();
        assert!(is_entirely_reference_team(&restyled, &ECOMMERCE));
    }

    /// It must not fire on a designed line-up. One added role is a decision the
    /// model made, and this guard exists to protect the provenance claim — not
    /// to police how much of the reference wording survived.
    #[test]
    fn one_role_of_its_own_is_enough_to_be_a_designed_team() {
        let mut roster = ECOMMERCE.proposed();
        roster.push(proposed("Cold Email Specialist"));
        assert!(!is_entirely_reference_team(&roster, &ECOMMERCE));

        // The real case this was checked against: three template roles and
        // three of the model's own is a designed team.
        let mixed = vec![
            proposed("SEO Specialist"),
            proposed("Logistics Coordinator"),
            proposed("Accountant"),
            proposed("Cold Email Specialist"),
            proposed("Product Researcher"),
            proposed("Social Media Manager"),
        ];
        assert!(!is_entirely_reference_team(&mixed, &ECOMMERCE));
    }

    /// An empty roster is not a copy of anything. Reported as false so the
    /// caller's own too-thin check stays the thing that handles it — two rules
    /// competing over one case is how the padding bug happened.
    #[test]
    fn an_empty_roster_is_not_a_copy() {
        assert!(!is_entirely_reference_team(&[], &ECOMMERCE));
    }

    /// The hole a prompt-injection test found: an **invalid** focus used to
    /// produce a wider agent than any valid one, because an empty `tools` list is
    /// read as "inherit the company belt" and that belt is
    /// `["*", "media", "composio"]`.
    ///
    /// Quantified over the whole vocabulary plus the unknown case, so the
    /// invariant is "no focus, recognised or not, out-grants another" rather than
    /// four separate assertions about four lists.
    #[test]
    fn an_unrecognised_focus_can_never_out_grant_a_recognised_one() {
        const FORBIDDEN: [&str; 5] = ["media", "composio", "search", "repo", "shell"];
        let unknown = tools_for_focus(AgentFocus::from_wire("media"));
        assert!(!unknown.is_empty());
        for grant in &unknown {
            let namespace = grant.split(['.', '_', ':']).next().unwrap_or(grant);
            assert!(
                !FORBIDDEN.contains(&namespace),
                "unknown focus grants {grant}"
            );
            assert_ne!(grant, "*");
        }
        // And the belt it lands on is one a real focus already has, not a
        // bespoke list that could drift away from the vocabulary.
        assert!(
            AgentFocus::ALL.iter().any(|f| f.tools() == unknown),
            "the fallback belt must be one of the real ones: {unknown:?}"
        );
    }

    /// The whole point, end to end: a roster whose focus values were tampered
    /// with still yields agents that ask for a belt rather than inheriting one.
    #[test]
    fn a_tampered_focus_still_narrows_the_agent() {
        let wire = r#"[
            {"name":"A","role":"Ops","description":"d","focus":"media"},
            {"name":"B","role":"Money","description":"d","focus":"composio"},
            {"name":"C","role":"Writer","description":"d"}
        ]"#;
        let roster: Vec<ProposedAgent> = serde_json::from_str(wire).expect("parses");
        let manifest = manifest_from_setup(&answers("a shop", ""), &roster, None);
        for agent in &manifest.agents {
            assert!(!agent.tools.is_empty(), "{} inherits the lot", agent.id);
            assert!(
                !agent
                    .tools
                    .iter()
                    .any(|t| t == "media" || t == "composio" || t == "*"),
                "{} holds {:?}",
                agent.id,
                agent.tools
            );
        }
        assert_eq!(manifest.validate(), Vec::<String>::new());
    }

    // ---------------------------------------------------------------------
    // The admin address, and the console that must agree about it
    // ---------------------------------------------------------------------

    /// The rule the console re-implements, pinned to a shared fixture.
    ///
    /// A wizard that let `as` through produced a company whose manifest failed
    /// validation on the *last* screen, after the roster had been designed and
    /// the apply attempted — the operator was told "that didn't apply" about a
    /// mistake they made four steps earlier.
    ///
    /// The console cannot call this validator, so it re-implements the rule, and
    /// this fixture is what stops the two drifting. Deliberately loose on the
    /// host side: `normalize_email` is trim + lowercase and the only structural
    /// demand is an `@`, because the rule exists to stop an entry normalizing
    /// into something `LoginIdentity::parse` would misread — not to police what
    /// a mail server accepts. A console applying a stricter regex would reject
    /// addresses the host takes happily.
    #[test]
    fn the_admin_address_rule_matches_the_shared_fixture() {
        #[derive(serde::Deserialize)]
        struct Case {
            why: String,
            input: String,
            usable: bool,
        }
        #[derive(serde::Deserialize)]
        struct Fixture {
            cases: Vec<Case>,
        }

        let raw = include_str!("../../tests/fixtures/setup-admin-email.json");
        let fixture: Fixture = serde_json::from_str(raw).expect("fixture parses");
        assert!(
            !fixture.cases.is_empty(),
            "an empty fixture asserts nothing"
        );

        for case in &fixture.cases {
            assert_eq!(
                crate::ports::users::is_usable_admin_email(&case.input),
                case.usable,
                "{} — input {:?}",
                case.why,
                case.input
            );
        }

        // And the manifest validator applies the same rule, not a second one:
        // every address the predicate rejects must be refused when written.
        for case in fixture
            .cases
            .iter()
            .filter(|c| !c.usable && !c.input.trim().is_empty())
        {
            let manifest = manifest_from_setup(
                &answers("a shop", ""),
                &[proposed("Ops")],
                Some(&case.input),
            );
            assert!(
                manifest
                    .validate()
                    .iter()
                    .any(|p| p.contains("[users].admins")),
                "{} — {:?} reached a valid manifest",
                case.why,
                case.input
            );
        }
    }
}
