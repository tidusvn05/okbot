use std::path::PathBuf;

use super::*;
use crate::write::{ZaloInit, upsert_env};

fn options(delivery: Delivery) -> InitOptions {
    InitOptions {
        name: "Shop bot".into(),
        personal: false,
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
        telegram: None,
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
    assert!(config.schedule.enabled);
    assert_eq!(config.schedule.late_grace_minutes, 360);
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
fn commands_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("okbot.toml"),
        "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n\
         [[command]]\nname = \"logs\"\ndescription = \"Errors\"\nmcp = \"ops\"\ntool = \"company_logs\"\n\
         args = [\"company\", \"service\", \"since?\"]\nwith = { level = \"error\", limit = 50 }\n",
    )
    .unwrap();
    let config = Config::load(dir.path()).unwrap();
    let c = &config.commands[0];
    assert_eq!((c.name.as_str(), c.tool.as_str()), ("logs", "company_logs"));
    assert_eq!(c.args, ["company", "service", "since?"]);
    assert_eq!(c.with["limit"], toml::Value::Integer(50));
}

#[test]
fn mcp_servers_load_and_their_tokens_are_secrets() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://127.0.0.1:8790/mcp\"\ntoken_env = \"OPS_MCP_TOKEN\"\nadmins_only = true\n",
    );
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.mcp.len(), 1);
    assert_eq!(config.mcp[0].name, "ops");
    assert!(config.mcp[0].admins_only);
    assert_eq!(config.mcp[0].auth, McpAuth::Bearer);
    assert!(config.secret_names().contains(&"OPS_MCP_TOKEN".to_string()));
    write(
        dir.path(),
        "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://127.0.0.1:8790/mcp\"\ntoken_env = \"T\"\nauth = \"signed\"\n",
    );
    assert_eq!(
        Config::load(dir.path()).unwrap().mcp[0].auth,
        McpAuth::Signed
    );
}

#[test]
fn roles_limits_and_mcp_rules_load() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        r#"config_version = 1
[bot]
name = "x"

[people.lan]
accounts = ["discord:main/1"]

[roles]
ops = ["telegram:main/7", "person:lan"]
support = []

[limits]
turns_per_day = 50
tokens_per_day = 2000000

[[mcp]]
name = "ops"
url = "http://127.0.0.1:8790/mcp"
token_env = "OPS_MCP_TOKEN"
roles = ["ops", "admins"]
rooms = ["gchat:corp/spaces/AAA"]
tools = ["status", "logs_*"]
deny_tools = ["logs_purge"]
confirm = "destructive"
confirm_by = "admins"

[[mcp.restrict]]
tools = ["deploy*"]
roles = ["ops"]

[[command]]
name = "status"
mcp = "ops"
tool = "status"
roles = ["support"]
"#,
    );
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.roles["ops"], ["telegram:main/7", "person:lan"]);
    assert_eq!(config.roles["support"], Vec::<String>::new());
    assert_eq!(config.limits.turns_per_day, 50);
    assert_eq!(config.limits.tokens_per_day, 2_000_000);
    let m = &config.mcp[0];
    assert_eq!(m.roles, ["ops", "admins"]);
    assert_eq!(m.rooms, ["gchat:corp/spaces/AAA"]);
    assert_eq!(m.tools, ["status", "logs_*"]);
    assert_eq!(m.deny_tools, ["logs_purge"]);
    assert_eq!(m.restrict[0].tools, ["deploy*"]);
    assert_eq!(m.confirm, Confirm::Destructive);
    assert_eq!(m.confirm_by, ConfirmBy::Admins);
    assert_eq!(config.commands[0].roles, ["support"]);

    // Defaults: no limits, every tool, no confirmation.
    write(
        dir.path(),
        "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n",
    );
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.limits, LimitsSection::default());
    assert!(config.mcp[0].tools.is_empty() && config.mcp[0].roles.is_empty());
    assert_eq!(config.mcp[0].confirm, Confirm::Never);
}

#[test]
fn command_servers_load() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"gh\"\ncommand = [\"npx\", \"-y\", \"server-github\"]\nenv = [\"GITHUB_TOKEN\"]\n",
    );
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.mcp[0].command, ["npx", "-y", "server-github"]);
    assert!(config.secret_names().contains(&"GITHUB_TOKEN".to_string()));
    assert!(!config.secret_names().contains(&String::new()));
    for (text, expected) in [
        ("command = [\"x\"]\nurl = \"http://x\"\n", "takes no url"),
        ("command = [\"x\"]\nauth = \"signed\"\n", "takes no url"),
        ("command = [\"\"]\n", "a program"),
        (
            "command = [\"x\"]\nenv = [\"gh token\"]\n",
            "not a variable name",
        ),
        (
            "url = \"http://x\"\ntoken_env = \"T\"\nenv = [\"A\"]\n",
            "env is for command",
        ),
        ("", "url must start"),
    ] {
        write(
            dir.path(),
            &format!("config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"gh\"\n{text}"),
        );
        let error = Config::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected:?} not in {error:?}");
    }
}

#[test]
fn rejects_bad_roles_and_rules() {
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[roles]\nadmins = [\"discord:main/1\"]\n",
            "built in",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[roles]\nOps = []\n",
            "role name",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[roles]\nops = [\"1234\"]\n",
            "[roles] ops",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[roles]\nops = [\"person:nobody\"]\n",
            "not in [people]",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\nroles = [\"ops\"]\n",
            "role `ops` is not in [roles]",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\nrooms = [\"general\"]\n",
            "rooms",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\ndeny_tools = [\"drop db\"]\n",
            "not a tool name",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[mcp.restrict]]\ntools = [\"deploy\"]\nroles = []\n",
            "needs tools and roles",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\nconfirm = \"sometimes\"\n",
            "confirm",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[command]]\nname = \"s\"\nmcp = \"ops\"\ntool = \"t\"\nroles = [\"nope\"]\n",
            "role `nope`",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[command]]\nname = \"approve\"\nmcp = \"ops\"\ntool = \"t\"\n",
            "built-in",
        ),
    ];
    for (text, expected) in cases {
        write(dir.path(), text);
        let error = Config::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected:?} not in {error:?}");
    }
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
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[channels.telegram.shop]\ntoken_env = \"T\"\ndelivery = \"webhook\"\npublic_url = \"https://x/telegram/shop\"\n",
            "secret_env",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\ntimezone = \"Mars/Base\"\n",
            "IANA",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"kb\"\nurl = \"http://x\"\ntoken_env = \"T\"\n",
            "reserved",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"ftp://x\"\ntoken_env = \"T\"\n",
            "url",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"$T\"\n",
            "token_env",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://y\"\ntoken_env = \"T\"\n",
            "twice",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[command]]\nname = \"Status\"\nmcp = \"ops\"\ntool = \"t\"\n",
            "a-z, 0-9 and `_`",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[command]]\nname = \"help\"\nmcp = \"ops\"\ntool = \"t\"\n",
            "built-in",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[command]]\nname = \"s\"\nmcp = \"nope\"\ntool = \"t\"\n",
            "not an [[mcp]]",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[command]]\nname = \"s\"\nmcp = \"ops\"\ntool = \"t\"\nargs = [\"a?\", \"b\"]\n",
            "after an optional",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[command]]\nname = \"s\"\nmcp = \"ops\"\ntool = \"t\"\nargs = [\"a...\", \"b\"]\n",
            "last argument",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[command]]\nname = \"s\"\nmcp = \"ops\"\ntool = \"t\"\nargs = [\"level?\"]\nwith = { level = \"error\" }\n",
            "fixed by `with`",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[schedule]\ndeliver_to = \"discord\"\n",
            "deliver_to",
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
fn discord_slack_and_telegram_sections_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = options(Delivery::Polling);
    o.zalo = None;
    o.discord = Some(("main".into(), "DISCORD_TOKEN".into()));
    o.slack = Some((
        "acme".into(),
        "SLACK_APP_TOKEN".into(),
        "SLACK_BOT_TOKEN".into(),
    ));
    o.telegram = Some(("tg".into(), "TELEGRAM_BOT_TOKEN".into()));
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
    assert_eq!(names, ["discord", "slack", "telegram"]);
    assert_eq!(config.channels.telegram["tg"].delivery, Delivery::Polling);
    assert!(
        config
            .secret_names()
            .contains(&"TELEGRAM_BOT_TOKEN".to_string())
    );
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
        "config_version = 1\n[bot]\nname = \"x\"\n[channels.bridge.main]\nplatform = \"matrix\"\nenv = [\"MATRIX_TOKEN\"]\ndialect = \"plain\"\nedit = true\n[channels.bridge.main.settings]\npoll = 30\n",
    );
    let config = Config::load(dir.path()).unwrap();
    let b = &config.channels.bridge["main"];
    assert_eq!(b.platform, "matrix");
    assert_eq!(b.settings["poll"].as_integer(), Some(30));
    assert_eq!(b.dialect, okbot_core::MarkdownDialect::Plain);
    assert_eq!(config.channels.access()[0].0, "matrix");
    assert!(config.secret_names().contains(&"MATRIX_TOKEN".to_string()));
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

#[test]
fn notify_targets_load_and_their_tokens_are_secrets() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n\
         [channels.discord.ops]\ntoken_env = \"D\"\n[people.luan]\naccounts = [\"discord:ops/1\"]\n\
         [[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\nauth = \"signed\"\n\
         [[notify]]\nname = \"alerts\"\ntoken_env = \"T\"\n\
         to = [\"room:discord:ops|42|7|-\", \"admins\", \"person:luan\", \"user:discord:ops/2\"]\n",
    );
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.notify[0].name, "alerts");
    assert_eq!(config.notify[0].to.len(), 4);
    // A signed [[mcp]] secret never reaches the agent, so callers may share it.
    assert!(config.secret_names().contains(&"T".to_string()));
}

#[test]
fn rejects_bad_notify_targets() {
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[[notify]]\nname = \"alerts\"\ntoken_env = \"N\"\nto = [\"admins\"]\n",
            "[http] listen",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n[channels.discord.ops]\ntoken_env = \"D\"\n[[notify]]\nname = \"Alerts\"\ntoken_env = \"N\"\nto = [\"admins\"]\n",
            "name `Alerts`",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n[channels.discord.ops]\ntoken_env = \"D\"\n[[notify]]\nname = \"a\"\ntoken_env = \"N\"\nto = [\"admins\"]\n[[notify]]\nname = \"a\"\ntoken_env = \"N\"\nto = [\"admins\"]\n",
            "defined twice",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n[channels.discord.ops]\ntoken_env = \"D\"\n[[notify]]\nname = \"a\"\ntoken_env = \"n\"\nto = [\"admins\"]\n",
            "token_env must be",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n[channels.discord.ops]\ntoken_env = \"D\"\n[[mcp]]\nname = \"ops\"\nurl = \"http://x\"\ntoken_env = \"T\"\n[[notify]]\nname = \"a\"\ntoken_env = \"T\"\nto = [\"admins\"]\n",
            "agents get",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n[channels.discord.ops]\ntoken_env = \"D\"\n[[notify]]\nname = \"a\"\ntoken_env = \"N\"\nto = []\n",
            "no target",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n[channels.discord.ops]\ntoken_env = \"D\"\n[[notify]]\nname = \"a\"\ntoken_env = \"N\"\nto = [\"everyone\"]\n",
            "`everyone`",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n[channels.discord.ops]\ntoken_env = \"D\"\n[[notify]]\nname = \"a\"\ntoken_env = \"N\"\nto = [\"person:luan\"]\n",
            "not in [people]",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n[channels.discord.ops]\ntoken_env = \"D\"\n[[notify]]\nname = \"a\"\ntoken_env = \"N\"\nto = [\"user:zalo:shop/1\"]\n",
            "not in [channels]",
        ),
        (
            "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n[channels.discord.ops]\ntoken_env = \"D\"\n[[notify]]\nname = \"a\"\ntoken_env = \"N\"\nto = [\"room:slack:corp|T1|C1|-\"]\n",
            "not in [channels]",
        ),
    ];
    for (text, expected) in cases {
        write(dir.path(), text);
        let error = Config::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected:?} not in {error:?}");
    }
}

const OAUTH_BASE: &str = r#"config_version = 1
[bot]
name = "x"
profile = "personal"

[http]
listen = "127.0.0.1:8787"
public_url = "https://bot.example.com"

[channels.discord.main]
token_env = "DISCORD_TOKEN"

[[mcp]]
name = "google"
url = "http://127.0.0.1:8000/mcp"
auth = "oauth"
confirm = "destructive"

[[mcp]]
name = "notion"
url = "https://mcp.notion.com/mcp"
auth = "oauth"
credentials = "shared"

[mcp.oauth]
client_id = "abc"
client_secret_env = "NOTION_CLIENT_SECRET"
scopes = ["read"]
"#;

#[test]
fn oauth_servers_load() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), OAUTH_BASE);
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.mcp[0].auth, McpAuth::Oauth);
    assert_eq!(config.mcp[0].credentials, McpCredentials::Person);
    assert_eq!(config.mcp[1].credentials, McpCredentials::Shared);
    let oauth = config.mcp[1].oauth.as_ref().unwrap();
    assert_eq!(oauth.client_id.as_deref(), Some("abc"));
    assert!(
        config
            .secret_names()
            .contains(&"NOTION_CLIENT_SECRET".to_string())
    );
    assert_eq!(
        config.http.public_url.as_deref(),
        Some("https://bot.example.com")
    );
}

#[test]
fn oauth_mistakes_are_explained() {
    let dir = tempfile::tempdir().unwrap();
    for (from, to, expected) in [
        (
            "auth = \"oauth\"\nconfirm",
            "auth = \"oauth\"\ntoken_env = \"T\"\nconfirm",
            "takes no token_env",
        ),
        (
            "credentials = \"shared\"",
            "credentials = \"shared\"\nauth = \"signed\"\ntoken_env = \"T\"",
            "duplicate key",
        ),
        ("client_id = \"abc\"\n", "", "needs client_id"),
        (
            "scopes = [\"read\"]",
            "scopes = [\"a b\"]",
            "is not a scope",
        ),
        (
            "public_url = \"https://bot.example.com\"",
            "public_url = \"http://bot.example.com\"",
            "must start with https://",
        ),
    ] {
        write(dir.path(), &OAUTH_BASE.replacen(from, to, 1));
        let error = Config::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected}: {error}");
    }
    let bearer_with_oauth = "config_version = 1\n[bot]\nname = \"x\"\n[[mcp]]\nname = \"ops\"\n\
        url = \"http://x\"\ntoken_env = \"T\"\ncredentials = \"shared\"\n";
    write(dir.path(), bearer_with_oauth);
    let error = Config::load(dir.path()).unwrap_err().to_string();
    assert!(error.contains("are for auth = \"oauth\""), "{error}");
}

#[test]
fn automations_load_with_their_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!(
        r#"{OAUTH_BASE}
[[automation]]
name = "boss-mail"
every = "5m"
prompt = "Summarise the new mail."

[automation.source]
mcp = "google"
tool = "search_gmail_messages"
with = {{ query = "is:unread from:boss@example.com" }}
items = "/messages"
id = "/id"

[[automation]]
name = "form"
webhook = true
token_env = "FORM_HOOK_TOKEN"
text = "New answer: {{{{item}}}}"
to = ["person:owner"]

[[automation]]
name = "morning"
schedule = "weekdays 08:00"
text = "Good morning"
to = ["admins"]
"#
    );
    write(dir.path(), &text);
    let config = Config::load(dir.path()).unwrap();
    let mail = &config.automations[0];
    assert_eq!(mail.every.as_deref(), Some("5m"));
    assert_eq!(mail.max_items, 20);
    assert_eq!(
        config.automation_run_as(mail).as_deref(),
        Some("person:owner"),
        "a personal bot runs automations as its owner"
    );
    assert_eq!(
        mail.source.as_ref().unwrap().with["query"],
        toml::Value::String("is:unread from:boss@example.com".into())
    );
    assert!(config.automations[1].webhook);
    assert!(
        config
            .secret_names()
            .contains(&"FORM_HOOK_TOKEN".to_string())
    );
    assert_eq!(parse_every("90").unwrap().as_secs(), 90 * 60);
    assert_eq!(parse_every("2h").unwrap().as_secs(), 7200);
    assert!(parse_every("0m").is_err() && parse_every("8d").is_err() && parse_every("x").is_err());
}

#[test]
fn automation_mistakes_are_explained() {
    let dir = tempfile::tempdir().unwrap();
    let base = "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n\
        [people.lan]\naccounts = [\"discord:main/1\"]\n\
        [channels.discord.main]\ntoken_env = \"D\"\n\
        [[mcp]]\nname = \"google\"\nurl = \"http://x\"\nauth = \"oauth\"\n\
        [[mcp]]\nname = \"local\"\ncommand = [\"x\"]\n";
    for (automation, expected) in [
        (
            "name = \"a\"\ntext = \"t\"\nto = [\"admins\"]\n",
            "exactly one trigger",
        ),
        (
            "name = \"a\"\nevery = \"5m\"\nschedule = \"daily 08:00\"\ntext = \"t\"\nto = [\"admins\"]\n",
            "exactly one trigger",
        ),
        (
            "name = \"a\"\nevery = \"5m\"\ntext = \"t\"\nto = [\"admins\"]\n",
            "add [automation.source]",
        ),
        (
            "name = \"a\"\nschedule = \"hourly\"\ntext = \"t\"\nto = [\"admins\"]\n",
            "not a repeat rule",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\ntext = \"t\"\n",
            "text needs `to`",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\ntext = \"t\"\nprompt = \"p\"\nto = [\"admins\"]\n",
            "either text or prompt",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\nprompt = \"p\"\n",
            "needs run_as",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\nprompt = \"p\"\nrun_as = \"person:bob\"\n",
            "not in [people]",
        ),
        (
            "name = \"a\"\nwebhook = true\ntext = \"t\"\nto = [\"admins\"]\n",
            "a webhook needs token_env",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\ntext = \"t\"\nto = [\"admins\"]\n[automation.source]\nmcp = \"google\"\ntool = \"x\"\n",
            "acts for a person; set run_as",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\ntext = \"t\"\nto = [\"admins\"]\n[automation.source]\nmcp = \"local\"\ntool = \"x\"\n",
            "is a command server",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\nrun_as = \"person:lan\"\ntext = \"t\"\nto = [\"person:lan\"]\n[automation.source]\nmcp = \"google\"\ntool = \"x\"\nitems = \"messages\"\n",
            "must be a JSON pointer",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\nprompt = \"p\"\nrun_as = \"person:lan\"\nto = [\"room:discord:main|-|1|-\", \"admins\"]\n",
            "names more",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\ntext = \"t\"\nto = [\"room:slack:x|-|1|-\"]\n",
            "not in [channels]",
        ),
        (
            "name = \"a\"\nschedule = \"daily 08:00\"\nrun_as = \"person:lan\"\ntext = \"t\"\nto = [\"room:discord:main|-|team|-\"]\n[automation.source]\nmcp = \"google\"\ntool = \"x\"\n",
            "may only go to them",
        ),
    ] {
        write(dir.path(), &format!("{base}[[automation]]\n{automation}"));
        let error = Config::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn teams_load_with_their_defaults_and_refuse_admins() {
    let dir = tempfile::tempdir().unwrap();
    let base = "config_version = 1\n[bot]\nname = \"x\"\n[roles]\nops = []\n";
    let config = Config::from_text(
        dir.path(),
        &format!("{base}[teams.local]\naccept = true\nroles = {{ main = [\"ops\"] }}\n"),
    )
    .unwrap();
    let team = &config.teams["local"];
    assert!(team.accept);
    assert_eq!(
        team.delegate_roles,
        ["admins"],
        "only admins delegate by default"
    );
    assert_eq!((team.max_depth, team.max_concurrent, team.wait), (2, 2, 60));
    for (team, expected) in [
        (
            "roles = { main = [\"admins\"] }",
            "never given to another bot",
        ),
        ("roles = { main = [\"nobody\"] }", "not in [roles]"),
        ("manifest = \"https://x/acme.toml\"", "needs `team_key`"),
        ("manifest = \"http://x/acme.toml\"", "must be https://"),
        ("confirm = \"destructive\"", "`never` or `always`"),
        ("max_depth = 0", "max_depth"),
        ("member = \"Lead\"", "may only contain"),
    ] {
        let error = Config::from_text(dir.path(), &format!("{base}[teams.acme]\n{team}\n"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{team}: {error}");
    }
}

#[test]
fn a2a_agents_and_clients_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let base = "config_version = 1\n[bot]\nname = \"x\"\n[http]\nlisten = \"127.0.0.1:8787\"\n\
        [people.lan]\naccounts = [\"discord:main/1\"]\n[roles]\nops = []\n";
    let ok = format!(
        "{base}[a2a]\ndelegate_roles = [\"everyone\"]\n\
         [[a2a.agent]]\nname = \"travel\"\nurl = \"https://agent.example.com/a2a\"\nversion = \"0.3\"\n\
         [[a2a.client]]\nname = \"n8n\"\ntoken_env = \"N8N_TOKEN\"\nroles = [\"ops\"]\nperson = \"lan\"\n"
    );
    let config = Config::from_text(dir.path(), &ok).unwrap();
    assert_eq!(config.a2a.agent[0].version, "0.3");
    assert_eq!(config.a2a.client[0].person.as_deref(), Some("lan"));
    for (section, expected) in [
        (
            "[[a2a.agent]]\nname = \"t\"\nurl = \"http://x\"\n",
            "https://",
        ),
        (
            "[[a2a.agent]]\nname = \"t\"\nurl = \"https://x\"\nversion = \"2\"\n",
            "`1.0` or `0.3`",
        ),
        (
            "[[a2a.client]]\nname = \"c\"\ntoken_env = \"T\"\nroles = [\"admins\"]\n",
            "never given",
        ),
        (
            "[[a2a.client]]\nname = \"c\"\ntoken_env = \"T\"\nperson = \"bob\"\n",
            "not in [people]",
        ),
        (
            "[[a2a.client]]\nname = \"c\"\ntoken_env = \"\"\n",
            "needs token_env",
        ),
        ("[teams.external]\n", "reserved"),
    ] {
        let error = Config::from_text(dir.path(), &format!("{base}{section}"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{section}: {error}");
    }
    let no_http = "config_version = 1\n[bot]\nname = \"x\"\n[[a2a.client]]\nname = \"c\"\ntoken_env = \"T\"\n";
    let error = Config::from_text(dir.path(), no_http)
        .unwrap_err()
        .to_string();
    assert!(error.contains("[http] listen"), "{error}");
}
