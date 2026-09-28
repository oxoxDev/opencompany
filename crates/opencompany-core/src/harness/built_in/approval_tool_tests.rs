use super::*;
use crate::harness::policy::ApprovalScope;

#[tokio::test]
async fn calling_the_tool_queues_one_explicit_request_for_its_agent() {
    let queue = ApprovalRequestQueue::default();
    let claim = queue.claim(ApprovalScope::Cycle);
    let tool = RequestApprovalTool::new("finance", queue.clone());
    let args = json!({
        "title": "Send the filing",
        "question": "May I submit the signed filing?",
        "context": "Submission is irreversible."
    });

    let result = claim.scoped(tool.execute(args.clone())).await.unwrap();
    assert!(!result.is_error);
    assert!(result.output().contains("Stop now"));

    let drained = claim.drain(8);
    assert_eq!(drained.requests.len(), 1);
    let request = &drained.requests[0];
    assert_eq!(request.tool, REQUEST_APPROVAL_TOOL);
    assert_eq!(request.reason, "May I submit the signed filing?");
    assert_eq!(request.effect.agent.as_deref(), Some("finance"));
    assert_eq!(request.effect.payload, args);
}

#[tokio::test]
async fn blank_required_copy_is_refused_without_queueing_a_card() {
    let queue = ApprovalRequestQueue::default();
    let tool = RequestApprovalTool::new("finance", queue.clone());

    let error = tool
        .execute(json!({ "title": " ", "question": "Proceed?" }))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("`title`"));
    assert!(queue.drain(8).requests.is_empty());
}

#[tokio::test]
async fn a_request_nothing_can_record_is_an_error_and_queues_nothing() {
    let queue = ApprovalRequestQueue::default();
    let tool = RequestApprovalTool::new("finance", queue.clone());

    let result = tool
        .execute(json!({ "title": "Send the filing", "question": "May I submit it?" }))
        .await
        .unwrap();

    assert!(
        result.is_error,
        "an unrecorded request must not read as asked"
    );
    assert!(
        result.output().contains("was not recorded"),
        "{}",
        result.output()
    );
    assert!(result.output().contains("Do not tell anyone you asked"));
    let cycle = queue.claim(ApprovalScope::Cycle);
    assert!(cycle.drain(8).requests.is_empty());
}
