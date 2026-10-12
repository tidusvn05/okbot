//! Memory notes: visibility, safety, lint level and git history.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::sync::Arc;
use std::time::Duration;

use okbot_core::{
    ConversationKey, KnowledgeProvider, MemoryError, MemoryNote, MemoryScope, MemoryStore,
    Requester, UserRef,
};
use okbot_knowledge::{Knowledge, KnowledgeOptions, write_default_bundle};

fn who(user: &str, room: &str) -> Requester {
    Requester {
        conversation: ConversationKey::room("zalo:shop".parse().unwrap(), room),
        user: UserRef {
            id: user.into(),
            display_name: user.into(),
            is_bot: false,
        },
        internal: false,
    }
}

fn note(scope: MemoryScope, text: &str) -> MemoryNote {
    MemoryNote {
        scope,
        title: None,
        text: text.into(),
    }
}

fn open(git: bool) -> (tempfile::TempDir, Arc<Knowledge>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("kb");
    write_default_bundle(&root).unwrap();
    if git {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ]);
    }
    let mut options = KnowledgeOptions::new(&root);
    options.index_dir = Some(dir.path().join("index"));
    (dir, Arc::new(Knowledge::open(options).unwrap()))
}

#[tokio::test]
async fn notes_are_scoped_and_keep_the_bundle_clean() {
    let (_dir, k) = open(false);
    let lan = who("u1", "r1");
    let minh = who("u2", "r1");
    let other_room = who("u1", "r2");

    let mine = k
        .save(&lan, note(MemoryScope::User, "Lan thích trà sữa ít đường"))
        .await
        .unwrap();
    assert!(
        mine.id.starts_with("memory/users/zalo-shop/u1/"),
        "{}",
        mine.id
    );
    k.save(
        &lan,
        note(MemoryScope::Room, "This chat is about order #42"),
    )
    .await
    .unwrap();
    k.save(
        &lan,
        note(MemoryScope::Global, "The shop closes at 9 pm on Sundays"),
    )
    .await
    .unwrap();

    let texts = |entries: Vec<okbot_core::MemoryEntry>| -> Vec<String> {
        entries.into_iter().map(|e| e.text).collect()
    };
    let lan_sees = texts(k.list(&lan).await.unwrap());
    assert_eq!(lan_sees.len(), 3, "{lan_sees:?}");
    let minh_sees = texts(k.list(&minh).await.unwrap());
    assert!(
        !minh_sees.iter().any(|t| t.contains("trà sữa")),
        "{minh_sees:?}"
    );
    assert!(
        minh_sees.iter().any(|t| t.contains("order #42")),
        "same room"
    );
    let elsewhere = texts(k.list(&other_room).await.unwrap());
    assert!(
        !elsewhere.iter().any(|t| t.contains("order #42")),
        "{elsewhere:?}"
    );
    assert!(
        elsewhere.iter().any(|t| t.contains("9 pm")),
        "global notes are everywhere"
    );

    // The profile context carries the user's notes; the catalog lists global ones only.
    let ctx = k.context(&lan).await.unwrap();
    assert!(ctx.user_profile.unwrap().contains("trà sữa"));
    assert!(k.context(&minh).await.unwrap().user_profile.is_none());

    // Written notes keep the bundle at L2 with no warnings.
    let report = k
        .bundle()
        .lint(&okbase::LintConfig::level(okbase::Level::L2))
        .unwrap();
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["errors"], 0, "{json:#}");
    assert_eq!(json["warnings"], 0, "{json:#}");
}

#[tokio::test]
async fn forgetting_respects_scope_and_authorship() {
    let (_dir, k) = open(false);
    let lan = who("u1", "r1");
    let minh = who("u2", "r1");
    let personal = k
        .save(&lan, note(MemoryScope::User, "Lan's birthday is in May"))
        .await
        .unwrap();
    let team = k
        .save(&lan, note(MemoryScope::Global, "Team lunch is on Fridays"))
        .await
        .unwrap();

    assert!(matches!(
        k.forget(&minh, &personal.id).await,
        Err(MemoryError::NotFound(_))
    ));
    assert!(matches!(
        k.forget(&minh, &team.id).await,
        Err(MemoryError::Forbidden(_))
    ));
    for bad in [
        "../okbot.toml",
        "memory/users/zalo-shop/u1/../../../persona/assistant",
        "/etc/passwd",
        "",
    ] {
        assert!(k.forget(&lan, bad).await.is_err(), "{bad}");
    }
    assert!(k.bundle().root().join("persona/assistant.md").exists());

    k.forget(&lan, &personal.id).await.unwrap();
    k.forget(&lan, &team.id).await.unwrap();
    assert_eq!(k.list(&lan).await.unwrap(), []);
}

#[tokio::test]
async fn rejects_empty_and_oversized_notes() {
    let (_dir, k) = open(false);
    let lan = who("u1", "r1");
    assert!(matches!(
        k.save(&lan, note(MemoryScope::User, "  ")).await,
        Err(MemoryError::Invalid(_))
    ));
    let long = "x".repeat(2001);
    assert!(matches!(
        k.save(&lan, note(MemoryScope::User, &long)).await,
        Err(MemoryError::Invalid(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_saves_get_distinct_files() {
    let (_dir, k) = open(false);
    let mut tasks = Vec::new();
    for i in 0..12 {
        let k = k.clone();
        tasks.push(tokio::spawn(async move {
            k.save(
                &who("u1", "r1"),
                note(MemoryScope::User, &format!("same title {}", i % 2)),
            )
            .await
            .unwrap()
            .id
        }));
    }
    let mut ids = Vec::new();
    for t in tasks {
        ids.push(t.await.unwrap());
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 12);
}

#[tokio::test]
async fn writes_are_committed_when_the_bundle_is_a_git_repository() {
    let (_dir, k) = open(true);
    let lan = who("u1", "r1");
    let entry = k
        .save(
            &lan,
            note(MemoryScope::Global, "Wifi password changes monthly"),
        )
        .await
        .unwrap();
    k.forget(&lan, &entry.id).await.unwrap();
    let log = std::process::Command::new("git")
        .arg("-C")
        .arg(k.bundle().root())
        .args(["log", "--format=%an %s"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(
        log.contains(&format!("okbot memory: save {}", entry.id)),
        "{log}"
    );
    assert!(
        log.contains(&format!("okbot memory: forget {}", entry.id)),
        "{log}"
    );
}

#[tokio::test]
async fn grants_include_the_memory_server() {
    let (_dir, k) = open(false);
    k.start_mcp(okbot_core::CancellationToken::new())
        .await
        .unwrap();
    let grant = k
        .grant(
            &who("u1", "r1"),
            &okbot_core::RunInfo {
                tools: okbot_core::ToolPolicy::McpOnly,
                ttl: Duration::from_secs(60),
                attachments: &[],
            },
        )
        .await
        .unwrap();
    let names: Vec<&str> = grant.mcp.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["kb", "bot"]);
    assert!(grant.mcp[1].url.ends_with("/bot/mcp"));
    assert_eq!(grant.mcp[0].bearer_token_env, grant.mcp[1].bearer_token_env);
}
