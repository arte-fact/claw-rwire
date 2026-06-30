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

/// Fallback for a model that wrote its decision as prose instead of calling the tool: pull
/// `(decision, feedback)` from a `Verdict: <decision> — <feedback>` line (the form models like
/// deepseek actually emit). Looks at the *last* such line (the conclusion); `None` if there's no
/// recognisable decision word on it. Without this a perfectly good review is read as "no verdict".
#[must_use]
pub fn parse_verdict_prose(text: &str) -> Option<(String, String)> {
    let lower = text.to_lowercase();
    let after = &text[lower.rfind("verdict")? + "verdict".len()..];
    // The rest of the "Verdict…" line carries the decision (and usually the feedback).
    let line = &after[..after.find('\n').unwrap_or(after.len())];
    let line_lower = line.to_lowercase();
    let (at, decision) = ["pass", "revise", "reject"]
        .into_iter()
        .filter_map(|word| line_lower.find(word).map(|at| (at, word)))
        .min_by_key(|(at, _)| *at)?;
    let feedback = line[at + decision.len()..]
        .trim_start_matches([' ', ':', '-', '—', '–', '\t', '.'])
        .trim()
        .to_owned();
    Some((decision.to_owned(), feedback))
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

    #[test]
    fn prose_verdict_recovers_decision_and_feedback() {
        // The real deepseek shape: a precise review ending in a `Verdict:` line, no tool call.
        let text = "The inner match arm is wrong.\n\n\
                    Verdict: revise — change the inner `_ => continue` to `_ => return None`.";
        assert_eq!(
            parse_verdict_prose(text),
            Some((
                "revise".to_owned(),
                "change the inner `_ => continue` to `_ => return None`.".to_owned()
            ))
        );
        // A bare pass with no feedback still routes.
        assert_eq!(
            parse_verdict_prose("Looks correct against the spec. Verdict: pass"),
            Some(("pass".to_owned(), String::new()))
        );
    }

    #[test]
    fn prose_verdict_is_none_without_a_decision_word() {
        assert_eq!(parse_verdict_prose("I couldn't reach a verdict here"), None);
        assert_eq!(parse_verdict_prose("just some prose, no decision"), None);
    }
}
