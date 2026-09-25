//! Tests that the console repeats this module's operator-facing sentences
//! verbatim rather than paraphrasing them.

use super::{BUILTIN_UNINSTALL, SKILL_MODIFIED_NO_UPDATE};

/// The Skills list greys a built-in skill's Uninstall and prints the reason
/// beside it, rather than hiding the action — an affordance that vanishes
/// teaches nothing, and an operator who does not see Uninstall cannot learn
/// why it is not offered.
///
/// That reason has to be the sentence the route itself answers with. Two
/// wordings for one rule means the menu explains the refusal one way and the
/// toast another, and only one of them gets updated when the rule changes.
/// Read out of the console's own source so a drift on either side fails here.
#[test]
fn the_console_gives_the_same_reason_a_builtin_cannot_be_uninstalled() {
    const CONSOLE_LIB: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/skills-list.ts"
    ));
    assert!(
        CONSOLE_LIB.contains(BUILTIN_UNINSTALL),
        "frontend/src/lib/skills-list.ts no longer carries this module's \
         `BUILTIN_UNINSTALL` sentence verbatim:\n  {BUILTIN_UNINSTALL}\nThe console's greyed \
         Uninstall would explain the refusal in different words from the route that performs \
         it. Change both together."
    );
}

/// The same discipline for the refusal an operator can reach two ways.
///
/// The row menu greys Update on a modified skill and prints the reason; posting
/// the update anyway is refused by the route with `SKILL_MODIFIED_NO_UPDATE`.
/// Those are the same fact, and it is the one refusal that asks the operator to
/// do something specific instead — so two wordings would leak two different
/// explanations of what to do, and only one of them would be kept current.
#[test]
fn the_console_gives_the_same_reason_a_modified_skill_cannot_be_updated() {
    const CONSOLE_LIB: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/skills-list.ts"
    ));
    assert!(
        CONSOLE_LIB.contains(SKILL_MODIFIED_NO_UPDATE),
        "frontend/src/lib/skills-list.ts no longer carries this module's \
         `SKILL_MODIFIED_NO_UPDATE` sentence verbatim:\n  {SKILL_MODIFIED_NO_UPDATE}\nThe \
         greyed Update would explain the refusal in different words from the route that \
         performs it. Change both together."
    );
}
