//! What an episode withholds from a seat in an operator's direct line.

use super::*;

/// `broadcast` exists for a room, and a DM is not one.
///
/// Keyed on the episode's desk, not on the seat: the teammate answering the
/// operator is the one that reached for it in both live runs, and it is no
/// more addressable to a room than a guest is. A desk keeps it -- that is the
/// conversation it was built for.
#[test]
fn only_a_direct_line_withholds_broadcast() {
    assert_eq!(
        broadcast_withheld_in(true, "desk_").as_deref(),
        Some("desk_broadcast"),
        "withheld by its served name, which is what the belt is filtered on"
    );
    assert_eq!(
        broadcast_withheld_in(false, "desk_"),
        None,
        "a desk is a real room with a real audience"
    );
}
