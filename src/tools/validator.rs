//! The validator's `verdict` tool (Phase F-2). A loop step's validator agent reviews the
//! producer's output and calls `verdict(decision, feedback)` to route the loop — `pass`
//! (accept), `revise` (send back with feedback), or `reject` (unrecoverable). The decision
//! leaves the model as a structured tool call, not parsed prose, so the engine routes
//! reliably. The toolbox is stateless; the engine reads the verdict from the run's recorded
//! invocations via [`parse_verdict`].

use serde_json::{Value, json};

use crate::agent::ToolInvocation;

use super::{ToolDef, ToolOutcome, Toolbox};

pub const VERDICT: &str = "verdict";

/// A toolbox advertising only the `verdict` tool.
pub struct ValidatorToolbox;

impl Toolbox for ValidatorToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: VERDICT.to_owned(),
            description: "Record your review decision. Call this exactly once when you're done \
                          reviewing."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "decision": {
                        "type": "string",
                        "enum": ["pass", "revise", "reject"],
                        "description": "pass = meets the spec; revise = send back with feedback; reject = unrecoverable."
                    },
                    "feedback": {
                        "type": "string",
                        "description": "Concrete, actionable feedback. Required for revise/reject."
                    }
                },
                "required": ["decision"]
            }),
        }]
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        if name != VERDICT {
            return ToolOutcome::error(format!("error: no tool named {name}"));
        }
        let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        match args.get("decision").and_then(Value::as_str) {
            Some(decision @ ("pass" | "revise" | "reject")) => {
                ToolOutcome::text(format!("verdict recorded: {decision}"))
            }
            _ => ToolOutcome::error("error: decision must be pass, revise, or reject"),
        }
    }
}

/// Extract the verdict `(decision, feedback)` from a validator run's invocations — the most
/// recent valid `verdict` call. `None` if the validator never issued one.
#[must_use]
pub fn parse_verdict(invocations: &[ToolInvocation]) -> Option<(String, String)> {
    invocations
        .iter()
        .rev()
        .filter(|inv| inv.name == VERDICT)
        .find_map(|inv| {
            let value: Value = serde_json::from_str(&inv.arguments).ok()?;
            let decision = value.get("decision").and_then(Value::as_str)?;
            if !matches!(decision, "pass" | "revise" | "reject") {
                return None;
            }
            let feedback = value
                .get("feedback")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Some((decision.to_owned(), feedback))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv(name: &str, args: &str) -> ToolInvocation {
        ToolInvocation {
            name: name.to_owned(),
            arguments: args.to_owned(),
            result: String::new(),
            ok: true,
        }
    }

    #[test]
    fn parses_the_last_verdict() {
        let calls = vec![
            inv(VERDICT, r#"{"decision":"revise","feedback":"first"}"#),
            inv("read", r#"{"path":"x"}"#),
            inv(VERDICT, r#"{"decision":"pass"}"#),
        ];
        assert_eq!(
            parse_verdict(&calls),
            Some(("pass".to_owned(), String::new()))
        );
    }

    #[test]
    fn revise_keeps_the_feedback() {
        let calls = vec![inv(
            VERDICT,
            r#"{"decision":"revise","feedback":"fix the bug"}"#,
        )];
        assert_eq!(
            parse_verdict(&calls),
            Some(("revise".to_owned(), "fix the bug".to_owned()))
        );
    }

    #[test]
    fn no_verdict_or_bad_decision_is_none() {
        assert_eq!(parse_verdict(&[]), None);
        assert_eq!(
            parse_verdict(&[inv(VERDICT, r#"{"decision":"maybe"}"#)]),
            None
        );
        assert_eq!(parse_verdict(&[inv("read", "{}")]), None);
    }

    #[test]
    fn the_tool_validates_the_decision() {
        let tb = ValidatorToolbox;
        assert!(tb.call(VERDICT, r#"{"decision":"pass"}"#).ok);
        assert!(!tb.call(VERDICT, r#"{"decision":"nope"}"#).ok);
        assert!(!tb.call("other", "{}").ok);
    }
}
