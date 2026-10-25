//! Operator controls for agent discovery, configuration and intentional live probes.

use crate::exit::CliError;
use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use okbot_agent::{Provider, detect, model_parts, parse_provider, provider_name, validate_model};
use okbot_config::{Config, ConfigEditor};
use okbot_core::{AgentBackend, AgentTurn, ToolPolicy};
use serde_json::json;
use std::fmt::Write;
use std::path::PathBuf;
use std::time::Duration;

fn provider(value: &str) -> Result<String, String> {
    parse_provider(value)
        .map(|_| value.to_string())
        .ok_or_else(|| "use claude, codex, opencode or devin".into())
}

#[derive(Debug, Args)]
pub(crate) struct Target {
    /// Bot name or path; default: the bot in the current directory.
    #[arg(long)]
    bot: Option<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct EditOptions {
    #[command(flatten)]
    target: Target,
    /// Validate and print the proposed configuration without saving it.
    #[arg(long, conflicts_with = "restart")]
    print: bool,
    /// Restart the bot's systemd service after saving; foreground bots need a manual restart.
    #[arg(long)]
    restart: bool,
}

#[derive(Debug, Args)]
pub(crate) struct Settings {
    /// Model id or legacy model@effort. A new model keeps the previous effort unless overridden.
    #[arg(long, conflicts_with = "default_model")]
    model: Option<String>,
    /// Effort supported by this provider; requires an explicit or previously configured model.
    #[arg(long, conflicts_with_all = ["default_effort", "default_model"])]
    effort: Option<String>,
    /// Remove both model and effort overrides, using the CLI's own defaults.
    #[arg(long)]
    default_model: bool,
    /// Remove only the effort override.
    #[arg(long)]
    default_effort: bool,
    /// Exact CLI executable, checked and stored as an absolute path.
    #[arg(long, conflicts_with = "default_cli")]
    cli: Option<PathBuf>,
    /// Remove the executable override and use PATH discovery.
    #[arg(long)]
    default_cli: bool,
    #[command(flatten)]
    options: EditOptions,
}

#[derive(Debug, Args)]
pub(crate) struct UseArgs {
    /// Provider to promote to primary; existing agents remain as fallbacks.
    #[arg(value_parser = provider)]
    provider: String,
    #[command(flatten)]
    settings: Settings,
}

#[derive(Debug, Args)]
pub(crate) struct SetArgs {
    /// Provider to configure; defaults to the primary. Inactive provider settings are kept.
    #[arg(value_parser = provider)]
    provider: Option<String>,
    #[command(flatten)]
    settings: Settings,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentCommand {
    /// Find known CLIs and check version/login without a model call; outside a bot uses your login.
    Detect(Target),
    /// Show the configured chain, model/effort, login and tool-policy readiness.
    Status(Target),
    /// Promote a primary agent, keeping the remaining agents as fallbacks.
    Use(Box<UseArgs>),
    /// Set or clear per-provider model, effort and executable overrides.
    Set(Box<SetArgs>),
    /// Replace the fallback order; preserve all provider overrides for later reuse.
    Chain {
        /// Ordered comma-separated providers (`claude,codex`).
        #[arg(value_delimiter = ',', value_parser = provider)]
        providers: Vec<String>,
        #[command(flatten)]
        options: EditOptions,
    },
    /// Make one tiny live model call (consumes quota), without fallback or access to bot data.
    Test {
        /// Provider to test; defaults to the primary.
        #[arg(value_parser = provider)]
        provider: Option<String>,
        #[command(flatten)]
        target: Target,
    },
}

impl AgentCommand {
    fn target(&self) -> &Target {
        match self {
            Self::Detect(t) | Self::Status(t) | Self::Test { target: t, .. } => t,
            Self::Use(a) => &a.settings.options.target,
            Self::Set(a) => &a.settings.options.target,
            Self::Chain { options, .. } => &options.target,
        }
    }
    pub(crate) fn json(&self) -> bool {
        self.target().json
    }
}

async fn inspect(config: Option<&Config>, name: &str) -> Option<detect::DetectedCli> {
    let kind = parse_provider(name)?;
    let env = config.map(crate::bot::agent_env).unwrap_or_default();
    match config.and_then(|c| {
        c.agent
            .providers
            .get(name)
            .and_then(|p| p.cli.as_ref())
            .map(|p| c.path(p))
    }) {
        Some(path) => detect::detect_path(kind, &path, &env).await,
        None => detect::detect_one_with_env(kind, &detect::SearchPaths::from_env(), &env).await,
    }
}

async fn status(config: Option<&Config>, all: bool, json_output: bool) -> anyhow::Result<()> {
    let names = if all {
        okbot_agent::PROVIDERS
            .iter()
            .map(|p| provider_name(*p).to_string())
            .collect()
    } else {
        config.context("status needs a bot")?.agent.chain.clone()
    };
    let mut items = Vec::new();
    let mut text = String::new();
    let mut findings = false;
    for name in names {
        let kind = parse_provider(&name).context("validated provider")?;
        let detected = inspect(config, &name).await;
        let settings = config.and_then(|c| c.agent.providers.get(&name));
        let spec = settings.and_then(|s| s.model.as_deref());
        let (model, effort) = spec.map_or((None, None), |m| {
            let (m, e) = model_parts(kind, m);
            (Some(m), e)
        });
        let enforced = matches!(kind, Provider::Claude | Provider::Codex);
        let model_error = spec
            .and_then(|m| validate_model(kind, m).err())
            .map(|e| e.to_string());
        let state = match detected.as_ref().map(|d| d.login) {
            None => "missing",
            Some(detect::Login::LoggedOut) => "logged_out",
            _ if model_error.is_some() => "invalid_model",
            _ if config.is_some_and(|c| !c.agent.allow_unrestricted_agents) && !enforced => {
                "blocked_by_tool_policy"
            }
            Some(detect::Login::Unknown) => "login_unknown",
            Some(detect::Login::LoggedIn) => "ready",
        };
        findings |= !matches!(state, "ready" | "login_unknown");
        let path = detected.as_ref().map(|d| d.path.clone()).or_else(|| {
            config.and_then(|c| settings.and_then(|s| s.cli.as_ref()).map(|p| c.path(p)))
        });
        let version = detected.as_ref().map(|d| d.version.as_str());
        let login = detected.as_ref().map(|d| match d.login {
            detect::Login::LoggedIn => "logged_in",
            detect::Login::LoggedOut => "logged_out",
            detect::Login::Unknown => "unknown",
        });
        let position = config.and_then(|c| c.agent.chain.iter().position(|p| p == &name));
        writeln!(
            text,
            "{name}: {state}{}\n  cli={}  version={}\n  model={}  effort={}  enforced_tools={enforced}",
            if position == Some(0) {
                " (primary)"
            } else {
                ""
            },
            path.as_ref()
                .map_or_else(|| "not found".into(), |p| p.display().to_string()),
            version.unwrap_or("unknown"),
            model.unwrap_or("CLI default"),
            effort.unwrap_or("CLI default")
        )?;
        if let Some(error) = &model_error {
            writeln!(text, "  {error}")?;
        }
        items.push(
            json!({"provider": name, "position": position, "status": state,
            "path": path, "version": version, "login": login, "model": model, "effort": effort,
            "model_spec": spec, "enforces_tools": enforced, "error": model_error}),
        );
    }
    crate::operator::emit(
        json!({"ok": all || !findings,
        "bot": config.map(|c| &c.bot.name), "agents": items}),
        &text,
        json_output,
        &crate::operator::redactor(config),
    );
    if !all && findings {
        return Err(CliError::reported_findings(
            "configured agents have findings; check login, executable and tool policy",
        )
        .into());
    }
    Ok(())
}

fn validate_settings(config: &Config) -> anyhow::Result<()> {
    for (name, settings) in &config.agent.providers {
        if let Some(model) = &settings.model {
            validate_model(parse_provider(name).context("validated provider")?, model)?;
        }
    }
    if !config.agent.allow_unrestricted_agents
        && !config
            .agent
            .chain
            .iter()
            .any(|n| matches!(n.as_str(), "claude" | "codex"))
    {
        bail!("the chain cannot enforce the bot's tool policy; include claude or codex");
    }
    Ok(())
}

async fn settings(
    config: &Config,
    editor: &mut ConfigEditor,
    name: &str,
    args: &Settings,
    promote: bool,
) -> anyhow::Result<()> {
    let kind = parse_provider(name).context("validated provider")?;
    let old = config
        .agent
        .providers
        .get(name)
        .cloned()
        .unwrap_or_default();
    let (old_model, old_effort) = old.model.as_deref().map_or((None, None), |m| {
        let (m, e) = model_parts(kind, m);
        (Some(m), e)
    });
    let mut model = args
        .model
        .as_deref()
        .map_or(old_model, |m| Some(model_parts(kind, m).0));
    let mut effort = args
        .model
        .as_deref()
        .and_then(|m| model_parts(kind, m).1)
        .or(old_effort);
    if let Some(e) = args.effort.as_deref() {
        effort = Some(e);
    }
    if args.default_effort {
        effort = None;
    }
    if args.default_model {
        model = None;
        effort = None;
    }
    if args.effort.is_some() && !matches!(kind, Provider::Claude | Provider::Codex) {
        bail!("{name} does not support --effort through agent-call");
    }
    if effort.is_some() && model.is_none() {
        return Err(CliError::needs(
            "--model",
            "setting effort requires a model; add --model MODEL_ID",
        )
        .into());
    }
    let model = model.map(|m| effort.map_or_else(|| m.to_string(), |e| format!("{m}@{e}")));
    if let Some(m) = &model {
        validate_model(kind, m)?;
    }
    if old.model != model {
        editor.set_agent_provider(name, "model", model.as_deref())?;
    }
    let mut binary = old.cli.clone();
    if args.default_cli {
        binary = None;
    }
    if let Some(path) = &args.cli {
        // Absolute but not symlink-resolved: `~/.local/bin/codex` stays valid when the CLI
        // updates itself into a new versioned directory.
        let path = std::path::absolute(path)
            .with_context(|| format!("cannot resolve {}", path.display()))?;
        if detect::detect_path(kind, &path, &crate::bot::agent_env(config))
            .await
            .is_none()
        {
            bail!("cannot run {} --version", path.display());
        }
        binary = Some(path);
    } else if promote && binary.is_none() && !args.default_cli {
        // Checked, not written: the bot finds it at each start.
        inspect(Some(config), name).await.with_context(|| {
            format!("{name} not found; run `okbot agent detect` or pass --cli PATH")
        })?;
    }
    if old.cli != binary {
        let text = binary.as_ref().map(|p| p.to_string_lossy());
        editor.set_agent_provider(name, "cli", text.as_deref())?;
    }
    if promote {
        if !config.agent.allow_unrestricted_agents
            && !matches!(kind, Provider::Claude | Provider::Codex)
        {
            bail!("{name} cannot enforce the bot's tool policy; choose claude or codex");
        }
        let mut chain = vec![name.to_string()];
        chain.extend(
            config
                .agent
                .chain
                .iter()
                .filter(|n| n.as_str() != name)
                .cloned(),
        );
        if chain != config.agent.chain {
            editor.set_agent_chain(&chain)?;
        }
    }
    Ok(())
}

fn save(config: &Config, editor: ConfigEditor, options: &EditOptions) -> anyhow::Result<()> {
    let candidate = editor.validate()?;
    validate_settings(&candidate)?;
    let preview = editor.text();
    let changed = preview != std::fs::read_to_string(config.dir.join(okbot_config::CONFIG_FILE))?;
    let r = crate::operator::redactor(Some(config));
    if options.print {
        crate::operator::emit(
            json!({"ok": true, "bot": config.bot.name, "saved": false,
            "changed": changed, "preview": preview}),
            &preview,
            options.target.json,
            &r,
        );
        return Ok(());
    }
    if changed {
        editor.save()?;
    }
    let restart = options
        .restart
        .then(|| crate::service::restart(&config.dir));
    let restarted = restart.as_ref().is_some_and(Result::is_ok);
    let restart_error = restart
        .and_then(Result::err)
        .map(|e| r.redact(&format!("{e:#}")).into_owned());
    let next = format!("okbot restart {}", crate::registry::name_of(&config.dir));
    let pending = (changed || options.restart) && !restarted;
    let mut text = format!(
        "{} configuration for {}\n",
        if changed { "saved" } else { "unchanged" },
        config.bot.name
    );
    if restarted {
        text.push_str("restarted the service\n");
    }
    if let Some(e) = &restart_error {
        writeln!(text, "restart failed: {e}")?;
    }
    if pending {
        writeln!(
            text,
            "apply to the running bot: {next} (foreground bots: restart your supervisor)"
        )?;
    }
    crate::operator::emit(
        json!({"ok": restart_error.is_none(), "bot": config.bot.name,
        "saved": true, "changed": changed, "restarted": restarted, "pending_restart": pending,
        "error": restart_error.as_ref().map(|m| json!({"code": "restart_failed", "message": m})),
        "next": if pending { vec![next] } else { vec![] }}),
        &text,
        options.target.json,
        &r,
    );
    if restart_error.is_some() {
        return Err(CliError {
            code: 1,
            message: "configuration saved; service restart failed".into(),
            flag: None,
            reported: true,
        }
        .into());
    }
    Ok(())
}

async fn test(mut config: Config, name: Option<String>, json_output: bool) -> anyhow::Result<()> {
    let name = name.unwrap_or_else(|| config.agent.chain[0].clone());
    config.agent.chain = vec![name.clone()];
    // A probe must enforce no shell/file access even for a development bot.
    config.agent.allow_unrestricted_agents = false;
    let result = async {
        let backend = crate::bot::agent_probe_chain(&config)?;
        let dir = tempfile::tempdir()?;
        tokio::time::timeout(
            Duration::from_secs(60),
            backend.run(AgentTurn {
                system: "Reply briefly. Use no tools.".into(),
                prompt: "Reply OK.".into(),
                tools: ToolPolicy::McpOnly,
                mcp: vec![],
                env: vec![],
                workdir: dir.path().to_path_buf(),
                timeout: Duration::from_secs(60),
                resume: None,
                progress: None,
            }),
        )
        .await
        .context("agent probe exceeded its 60-second deadline")?
        .map_err(anyhow::Error::from)
    }
    .await;
    let (ok, usage, duration, error) = match result {
        Ok(reply) => (
            !reply.text.trim().is_empty(),
            reply.usage,
            Some(reply.duration.as_millis()),
            None,
        ),
        Err(e) => (false, None, None, Some(format!("{e:#}"))),
    };
    let text = format!(
        "{name}: {}{}\n",
        if ok { "probe passed" } else { "probe failed" },
        error
            .as_ref()
            .map_or_else(String::new, |e| format!(": {e}"))
    );
    crate::operator::emit(
        json!({"ok": ok, "bot": config.bot.name, "provider": name,
        "usage": usage, "duration_ms": duration, "error": error}),
        &text,
        json_output,
        &crate::operator::redactor(Some(&config)),
    );
    if !ok {
        return Err(CliError::reported_findings("agent probe failed").into());
    }
    Ok(())
}

pub(crate) async fn run(command: AgentCommand) -> anyhow::Result<()> {
    let target = command.target();
    if matches!(command, AgentCommand::Detect(_)) {
        let config = if target.bot.is_some()
            || std::env::current_dir()?
                .join(okbot_config::CONFIG_FILE)
                .try_exists()?
        {
            Some(Config::load(&crate::registry::resolve(
                target.bot.as_deref(),
            )?)?)
        } else {
            None
        };
        return status(config.as_ref(), true, target.json).await;
    }
    let config = Config::load(&crate::registry::resolve(target.bot.as_deref())?)?;
    let redactor = crate::operator::redactor(Some(&config));
    let result = async { match command {
        AgentCommand::Status(t) => status(Some(&config), false, t.json).await,
        AgentCommand::Test { provider, target } => test(config, provider, target.json).await,
        AgentCommand::Use(a) => {
            let mut editor = ConfigEditor::open(&config.dir)?;
            settings(&config, &mut editor, &a.provider, &a.settings, true).await?;
            save(&config, editor, &a.settings.options)
        }
        AgentCommand::Set(a) => {
            let s = &a.settings;
            if s.model.is_none()
                && s.effort.is_none()
                && s.cli.is_none()
                && !s.default_model
                && !s.default_effort
                && !s.default_cli
            {
                bail!(
                    "provide --model, --effort, --cli or a --default-* flag; use `agent status` to inspect"
                );
            }
            let name = a.provider.as_deref().unwrap_or(&config.agent.chain[0]);
            let mut editor = ConfigEditor::open(&config.dir)?;
            settings(&config, &mut editor, name, s, false).await?;
            save(&config, editor, &s.options)
        }
        AgentCommand::Chain { providers, options } => {
            if providers.is_empty() {
                bail!("provide at least one provider, for example claude,codex");
            }
            let mut editor = ConfigEditor::open(&config.dir)?;
            if providers != config.agent.chain {
                editor.set_agent_chain(&providers)?;
            }
            save(&config, editor, &options)
        }
        AgentCommand::Detect(_) => unreachable!("detect handled above"),
    } }.await;
    result.map_err(|e| crate::operator::redact_error(&e, &redactor))
}
