//! Shared output for local operator commands.

use okbot_core::Redactor;
use serde_json::Value;

pub(crate) fn redactor(config: Option<&okbot_config::Config>) -> Redactor {
    let mut r = Redactor::new();
    if let Some(config) = config {
        for name in config.secret_names() {
            if let Ok(secret) = config.secret(&name) {
                r.add(&secret);
            }
        }
    }
    r
}

fn redact_value(value: &mut Value, r: &Redactor) {
    match value {
        Value::String(s) => *s = r.redact(s).into_owned(),
        Value::Array(values) => {
            for value in values {
                redact_value(value, r);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_value(value, r);
            }
        }
        _ => {}
    }
}

pub(crate) fn emit(mut value: Value, text: &str, json: bool, r: &Redactor) {
    if json {
        redact_value(&mut value, r);
        println!("{value}");
    } else {
        print!("{}", r.redact(text));
    }
}

pub(crate) fn redact_error(error: &anyhow::Error, r: &Redactor) -> anyhow::Error {
    let message = r.redact(&format!("{error:#}")).into_owned();
    if let Some(e) = error.downcast_ref::<crate::exit::CliError>() {
        crate::exit::CliError {
            code: e.code,
            flag: e.flag,
            reported: e.reported,
            message,
        }
        .into()
    } else {
        anyhow::anyhow!(message)
    }
}
