use super::*;

#[test]
fn extracts_plain_and_aliased_wikilinks_in_order() {
    let md =
        "See [[Spring launch]] and [[Brand voice|how we sound]], plus [[ Campaign checklist ]].";
    let links = extract_wikilinks(md);
    assert_eq!(
        links,
        vec!["Spring launch", "Brand voice", "Campaign checklist"]
    );
}

#[test]
fn ignores_empty_and_unterminated_links() {
    assert_eq!(
        extract_wikilinks("[[]] and [[unterminated"),
        Vec::<String>::new()
    );
    assert_eq!(
        extract_wikilinks("a [[|only alias]] b"),
        Vec::<String>::new()
    );
}

#[test]
fn walk_is_deterministic_and_markdown_only() {
    let dir = std::env::temp_dir().join(format!("oc-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("brand")).unwrap();
    std::fs::create_dir_all(dir.join("campaigns")).unwrap();
    std::fs::write(dir.join("readme.md"), "# Root\n[[Brand voice]]").unwrap();
    std::fs::write(dir.join("brand/brand-voice.md"), "# Voice").unwrap();
    std::fs::write(dir.join("campaigns/notes.txt"), "ignored").unwrap();
    std::fs::write(dir.join("cover.png"), b"\x89PNG").unwrap();

    let nodes = walk_workspace(&dir).unwrap();
    // Compared as paths, not as rendered strings. `rel_path` carries the
    // platform's separator, so `display()` yields `brand\brand-voice.md` on
    // Windows and the literal below never matched there. `Path` equality is
    // component-wise, which is the contract the walk actually owes: the same
    // components in the same order, whatever the platform writes between them.
    let paths: Vec<PathBuf> = nodes.iter().map(|n| n.rel_path.clone()).collect();
    // Sorted, folders + markdown only; the .txt and .png are skipped.
    assert_eq!(
        paths,
        vec![
            PathBuf::from("brand"),
            PathBuf::from("brand/brand-voice.md"),
            PathBuf::from("campaigns"),
            PathBuf::from("readme.md"),
        ]
    );

    let readme = nodes
        .iter()
        .find(|n| n.rel_path == Path::new("readme.md"))
        .unwrap();
    assert_eq!(readme.kind, NodeKind::Markdown);
    assert_eq!(readme.content.as_deref(), Some("# Root\n[[Brand voice]]"));

    let brand = nodes
        .iter()
        .find(|n| n.rel_path == Path::new("brand"))
        .unwrap();
    assert_eq!(brand.kind, NodeKind::Folder);
    assert_eq!(brand.content, None);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_directory_walks_to_empty() {
    let dir = std::env::temp_dir().join("oc-ws-does-not-exist-xyz");
    assert!(walk_workspace(&dir).unwrap().is_empty());
}

#[test]
fn path_traversal_is_rejected() {
    let root = Path::new("/tmp/workspace");
    let err = reject_escaping(root, Path::new("../secrets.md")).unwrap_err();
    assert_eq!(err.code(), "data_invalid");
    assert!(err.to_string().contains("escapes the workspace root"));
    // A normal nested path is accepted.
    assert!(reject_escaping(root, Path::new("brand/brand-voice.md")).is_ok());
}
