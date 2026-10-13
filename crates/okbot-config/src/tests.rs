use std::path::PathBuf;

use super::*;
use crate::write::{ZaloInit, upsert_env};

fn options(delivery: Delivery) -> InitOptions {
    InitOptions {
        name: "Shop bot".into(),
        chain: vec![
            (
                "claude".into(),
                Some(PathBuf::from("/home/u/.local/bin/claude")),
            ),
            ("codex".into(), None),
        ],
        zalo: Some(ZaloInit {
            instance: "shop".into(),
            token_env: "ZALO_BOT_TOKEN".into(),
            delivery,
            public_url: (delivery == Delivery::Webhook)
                .then(|| "https://bot.example.com/zalo/shop".into()),
            secret_env: (delivery == Delivery::Webhook).then(|| "ZALO_WEBHOOK_SECRET".into()),
        }),
        gchat: None,
        discord: None,
        slack: None,
        http_listen: (delivery == Delivery::Webhook).then(|| "127.0.0.1:8787".into()),
        timezone: Some("Asia/Ho_Chi_Minh".into()),
    }
}

fn write(dir: &std::path::Path, text: &str) {
    std::fs::write(dir.join(CONFIG_FILE), text).unwrap();
}

#[test]
fn initial_config_snapshot_and_round_trip() {
    let text = render_initial(&options(Delivery::Webhook));
    insta::assert_snapshot!(text);
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &text);
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.bot.name, "Shop bot");
    assert_eq!(config.agent.chain, ["claude", "codex"]);
    assert_eq!(config.agent.tool_policy, okbot_core::ToolPolicy::Web);
    assert_eq!(
        config.agent.providers["claude"].cli.as_deref(),
        Some(std::path::Path::new("/home/u/.local/bin/claude"))
    );
    let zalo = &config.channels.zalo["shop"];
    assert_eq!(zalo.delivery, Delivery::Webhook);
    assert_eq!(config.kb_dir(), dir.path().join("kb"));
}

#[test]
fn polling_config_loads() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &render_initial(&options(Delivery::Polling)));
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.channels.zalo["shop"].delivery, Delivery::Polling);
    assert!(config.http.listen.is_none());
}

#[test]
fn rejects_bad_configs() {
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        (
            "config_version = 2\n[bot]\nname = \"x\"\n",
            "config_version",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\ntypo = 1\n",
            "typo",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[agent]\nchain = [\"gpt\"]\n",
            "unknown agent",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[agent]\nchain = []\n",
            "at least one",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[channels.zalo.shop]\ntoken_env = \"T\"\ndelivery = \"webhook\"\n",
            "public_url",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[channels.zalo.Shop]\ntoken_env = \"T\"\n",
            "instance",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[channels.slack.a]\n",
            "slack",
        ),
    ];
    for (text, expected) in cases {
        write(dir.path(), text);
        let error = Config::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected:?} not in {error:?}");
    }
}

#[test]
fn secrets_come_from_env_file_and_never_print() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &render_initial(&options(Delivery::Polling)));
    let env = dir.path().join(ENV_FILE);
    upsert_env(&env, "ZALO_BOT_TOKEN", "old").unwrap();
    upsert_env(&env, "OTHER", "x").unwrap();
    upsert_env(&env, "ZALO_BOT_TOKEN", "123:abc").unwrap();
    let text = std::fs::read_to_string(&env).unwrap();
    assert_eq!(text.matches("ZALO_BOT_TOKEN").count(), 1, "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&env).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let config = Config::load(dir.path()).unwrap();
    let token = config.secret("ZALO_BOT_TOKEN").unwrap();
    assert_eq!(token.expose(), "123:abc");
    assert!(
        !format!("{config:?}").contains("123:abc"),
        "Debug must not show .env values"
    );
    assert!(
        config
            .secret_names()
            .contains(&"ZALO_BOT_TOKEN".to_string())
    );
    let missing = config
        .secret("NOPE_NOT_SET_ANYWHERE")
        .unwrap_err()
        .to_string();
    assert!(missing.contains(".env"), "{missing}");
}

#[test]
fn gchat_section_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = options(Delivery::Polling);
    o.zalo = None;
    o.gchat = Some(crate::GChatInit {
        instance: "corp".into(),
        credentials: "gchat-service-account.json".into(),
        delivery: GChatDelivery::Pubsub,
        subscription: Some("projects/p/subscriptions/okbot".into()),
        audience_url: None,
    });
    write(dir.path(), &render_initial(&o));
    let config = Config::load(dir.path()).unwrap();
    let g = &config.channels.gchat["corp"];
    assert_eq!(
        g.subscription.as_deref(),
        Some("projects/p/subscriptions/okbot")
    );
    assert_eq!(config.channels.access()[0].0, "gchat");
}

#[test]
fn discord_and_slack_sections_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = options(Delivery::Polling);
    o.zalo = None;
    o.discord = Some(("main".into(), "DISCORD_TOKEN".into()));
    o.slack = Some((
        "acme".into(),
        "SLACK_APP_TOKEN".into(),
        "SLACK_BOT_TOKEN".into(),
    ));
    write(dir.path(), &render_initial(&o));
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.channels.discord["main"].token_env, "DISCORD_TOKEN");
    assert_eq!(
        config.channels.slack["acme"].bot_token_env,
        "SLACK_BOT_TOKEN"
    );
    let names: Vec<&str> = config
        .channels
        .access()
        .iter()
        .map(|(p, _, _)| *p)
        .collect();
    assert_eq!(names, ["discord", "slack"]);
    assert!(
        config
            .secret_names()
            .contains(&"SLACK_APP_TOKEN".to_string())
    );
}

#[test]
fn bridge_section() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "config_version = 1\n[bot]\nname = \"x\"\n[channels.bridge.main]\nplatform = \"telegram\"\nenv = [\"TG_TOKEN\"]\ndialect = \"plain\"\nedit = true\n[channels.bridge.main.settings]\npoll = 30\n",
    );
    let config = Config::load(dir.path()).unwrap();
    let b = &config.channels.bridge["main"];
    assert_eq!(b.platform, "telegram");
    assert_eq!(b.settings["poll"].as_integer(), Some(30));
    assert_eq!(b.dialect, okbot_core::MarkdownDialect::Plain);
    assert_eq!(config.channels.access()[0].0, "telegram");
    assert!(config.secret_names().contains(&"TG_TOKEN".to_string()));
}

#[test]
fn bridge_cannot_shadow_a_built_in_platform() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "config_version = 1\n[bot]\nname = \"x\"\n[channels.bridge.main]\nplatform = \"slack\"\n",
    );
    let error = Config::load(dir.path()).unwrap_err().to_string();
    assert!(error.contains("built in"), "{error}");
}

#[test]
fn people_link_accounts() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "config_version = 1\n[bot]\nname = \"x\"\n[people.luan]\naccounts = [\"zalo:me/u1\", \"discord:me/123\"]\n",
    );
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.people["luan"].accounts.len(), 2);

    for (body, needle) in [
        ("[people.Luan]\naccounts = []\n", "may only contain"),
        (
            "[people.luan]\naccounts = [\"zalo:me\"]\n",
            "must look like",
        ),
        (
            "[people.a]\naccounts = [\"zalo:me/u1\"]\n[people.b]\naccounts = [\"zalo:me/u1\"]\n",
            "linked to both",
        ),
    ] {
        write(
            dir.path(),
            &format!("config_version = 1\n[bot]\nname = \"x\"\n{body}"),
        );
        let error = Config::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains(needle), "{error}");
    }
}
