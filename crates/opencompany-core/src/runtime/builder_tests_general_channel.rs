use super::tests_core::*;
use super::*;

const ROSTER: &str = r#"
[company]
name = "Roster Co"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "cto"
role = "Chief Technologist"
"#;

#[tokio::test]
async fn a_first_build_stores_general_with_the_whole_roster() {
    use crate::store::FsCompanyStore;

    let home_dir = tmp_home("oc-general-first-");
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("roster-co");
    RuntimeBuilder::new(home.clone(), parse(ROSTER))
        .with_id(id.clone())
        .build()
        .await
        .unwrap();

    let stored = FsCompanyStore::new(home).load(&id).await.unwrap().unwrap();
    assert_eq!(stored.general_channel.id, "general");
    assert_eq!(stored.general_channel.name, "General");
    let roster: Vec<String> = stored
        .effective_agents()
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert!(roster.starts_with(&["ceo".to_string(), "cto".to_string()]));
    assert_eq!(stored.general_channel.members, roster);
}

#[tokio::test]
async fn a_record_saved_without_general_is_backfilled_on_build() {
    use crate::ports::types::GeneralChannel;
    use crate::store::FsCompanyStore;

    let home_dir = tmp_home("oc-general-backfill-");
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("roster-co");
    RuntimeBuilder::new(home.clone(), parse(ROSTER))
        .with_id(id.clone())
        .build()
        .await
        .unwrap();

    let store = FsCompanyStore::new(home.clone());
    let mut legacy = store.load(&id).await.unwrap().unwrap();
    legacy.general_channel = GeneralChannel::default();
    legacy.retire_agent("cto");
    store.save(&legacy).await.unwrap();

    RuntimeBuilder::new(home, parse(ROSTER))
        .with_id(id.clone())
        .build()
        .await
        .unwrap();

    let rebuilt = store.load(&id).await.unwrap().unwrap();
    assert_eq!(rebuilt.general_channel.id, "general");
    let roster: Vec<String> = rebuilt
        .effective_agents()
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert!(!roster.contains(&"cto".to_string()));
    assert_eq!(rebuilt.general_channel.members, roster);
}
