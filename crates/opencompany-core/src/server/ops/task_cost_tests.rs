use super::*;
use crate::ports::runs::{RunRecord, RunStatus};
use crate::ports::tasks::TaskTitle;
use crate::ports::tasks::{TaskDeliverable, TaskPlanningUsage};
use crate::ports::types::{CompanyId, TokenUsage};

fn task(id: &str, parent: Option<&str>, planning_cost: f64) -> TaskRecord {
    TaskRecord {
        id: id.to_string(),
        title: TaskTitle::authored(id),
        note: None,
        column: "done".to_string(),
        priority: "low".to_string(),
        assignee: "ops".to_string(),
        updated_at_millis: 1,
        origin: None,
        parent_task_id: parent.map(str::to_string),
        output: None,
        plan: None,
        planning_attempts: (planning_cost > 0.0)
            .then(|| TaskPlanningUsage {
                at_millis: 2,
                usage: TokenUsage {
                    cost_usd: planning_cost,
                    ..TokenUsage::default()
                },
            })
            .into_iter()
            .collect(),
        deliverable: TaskDeliverable::Once,
        workflow_proposal: None,
        origin_run_id: None,
        origin_workflow_id: None,
        origin_message_seq: None,
        bounced: None,
    }
}

fn run(id: &str, task_id: &str, status: RunStatus, cost: f64) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        company: CompanyId::new("acme"),
        task_id: Some(task_id.to_string()),
        chat_id: None,
        agent_id: "ops".to_string(),
        attempt: 1,
        status,
        trigger_event_seq: None,
        thread_root: None,
        created_at_millis: 3,
        started_at_millis: Some(3),
        finished_at_millis: Some(4),
        error: None,
        usage: TokenUsage {
            cost_usd: cost,
            ..TokenUsage::default()
        },
        step_count: 0,
        workflow_run_id: None,
        node_id: None,
        episode_id: None,
        round_revision: None,
    }
}

#[test]
fn task_total_always_equals_timeline_costs_plus_child_totals() {
    let tasks = vec![
        task("parent", None, 0.1),
        task("child", Some("parent"), 0.05),
    ];
    let runs = vec![
        // Failed attempts count exactly like successful ones.
        run("failed", "parent", RunStatus::Failed, 0.2),
        run("success", "child", RunStatus::Succeeded, 0.3),
    ];
    let costs = reconcile(&tasks, &runs, &HashMap::new());
    let parent = costs.totals["parent"];
    let own_timeline: f64 = costs.entries["parent"]
        .iter()
        .map(|entry| entry.amount_usd)
        .sum();
    let children = costs.totals["child"].total_usd;

    assert!((parent.total_usd - (own_timeline + children)).abs() < 1e-12);
    assert!((parent.total_usd - 0.65).abs() < 1e-12);
    assert!((parent.own_usd - 0.3).abs() < 1e-12);
}

#[test]
fn zero_usage_creates_no_cost_line() {
    let tasks = vec![task("free", None, 0.0)];
    let runs = vec![run("free-run", "free", RunStatus::Succeeded, 0.0)];
    let costs = reconcile(&tasks, &runs, &HashMap::new());
    assert!(costs.entries["free"].is_empty());
    assert_eq!(costs.totals["free"].total_usd, 0.0);
}

#[test]
fn redacted_task_cost_is_explicit_and_not_zero() {
    let display = super::super::tasks::CostDisplay::new(8.25, false).expect("positive cost");
    let value = serde_json::to_value(display).expect("serialize cost");
    assert_eq!(value["hidden"], true);
    assert!(value.get("amountUsd").is_none(), "{value}");
}
