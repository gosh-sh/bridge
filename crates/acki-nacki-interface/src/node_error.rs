//! The node's own answer to a message it refused, kept free of the `tvm-sdk`
//! feature so it is testable in the default build.

use serde_json::Value;

/// The compute-phase exit code the node reported for a message it refused
/// before accepting it. The SDK files the node's answer under
/// `node_error.extensions.details` (`send_message_server_error`); the code is
/// a number or a numeric string there, and `null` when the node has none.
pub(crate) fn node_exit_code(data: &Value) -> Option<i64> {
    let code = data.pointer("/node_error/extensions/details/exit_code")?;
    code.as_i64()
        .or_else(|| code.as_str().and_then(|s| s.trim().parse().ok()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The error data of a message the node refused, as the SDK hands it
    /// over.
    fn refused(details: Value) -> Value {
        json!({
            "node_error": {
                "extensions": {
                    "code": "TVM_ERROR",
                    "message": "Failed to execute the message. Error occurred during the compute phase.",
                    "details": details,
                }
            }
        })
    }

    #[test]
    fn the_exit_code_of_a_refused_message_is_read_from_the_node_answer() {
        assert_eq!(
            node_exit_code(&refused(json!({ "exit_code": 244 }))),
            Some(244)
        );
        assert_eq!(
            node_exit_code(&refused(json!({ "exit_code": "244" }))),
            Some(244)
        );
    }

    #[test]
    fn a_node_answer_without_an_exit_code_has_none() {
        assert_eq!(node_exit_code(&refused(json!({ "exit_code": null }))), None);
        assert_eq!(node_exit_code(&refused(json!({}))), None);
        assert_eq!(node_exit_code(&json!({ "exit_code": 244 })), None);
    }
}
