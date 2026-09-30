use super::*;

#[tokio::test]
async fn add_agent_seats_the_new_teammate_in_general() {
    let company = CompanyId::new("acme");
    let store = Arc::new(MemStore::seeded(seeded_record(&company)));
    let root = tempfile::tempdir().expect("tempdir");
    let events: Arc<dyn EventLog> = Arc::new(crate::store::FsEventLog::new(root.path()));
    let tool = AddAgentTool::new(
        company.clone(),
        store.clone(),
        "ceo".to_string(),
        None,
        Vec::new(),
    )
    .with_events(Some(events.clone()));

    let result = tool
        .execute(json!({ "name": "Jamie", "role": "Data Entry" }))
        .await
        .expect("execute");
    assert!(!result.is_error, "{}", result.text());

    let record = store.load(&company).await.unwrap().expect("persisted");
    let hired = record.overlay_agents[0].id.clone();
    assert!(record.general_channel.members.contains(&hired));

    let changes: Vec<_> =
        events
            .read_from(&company, crate::ports::types::EventSeq::new(0), usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|stored| match stored.event {
                crate::ports::types::CompanyEvent::DeskMembersChanged {
                    desk_id, added, ..
                } if desk_id == crate::ports::general_channel::GENERAL_CHANNEL_ID => Some(added),
                _ => None,
            })
            .collect();
    assert_eq!(changes, vec![vec![hired]]);
}
