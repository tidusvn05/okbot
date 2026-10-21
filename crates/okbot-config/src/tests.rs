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
