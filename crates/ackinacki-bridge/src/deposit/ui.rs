//! What a deposit run tells the person watching it. Every step reports
//! through [`Ui`]; `run.rs` picks the renderer (terminal board, plain
//! lines, NDJSON) once.

/// One stage of a deposit run, in pipeline order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepId {
    /// Checks made before anything is sent.
    Preflight,
    /// Pairing with the wallet.
    Pair,
    /// The `approve` transaction.
    Approve,
    /// The `deposit` transaction.
    Deposit,
    /// Waiting for the deposit block to be final on the EVM chain.
    EvmConfirm,
    /// Waiting for the block anchor on Acki Nacki.
    Anchor,
    /// Building the deposit proof.
    Prove,
    /// Sending `finalizeDeposit`.
    Finalize,
    /// Confirming the credit on Acki Nacki.
    Credit,
}

impl StepId {
    /// Every step, in pipeline order.
    pub const ALL: [StepId; 9] = [
        StepId::Preflight,
        StepId::Pair,
        StepId::Approve,
        StepId::Deposit,
        StepId::EvmConfirm,
        StepId::Anchor,
        StepId::Prove,
        StepId::Finalize,
        StepId::Credit,
    ];

    /// The human-readable name of the step.
    pub fn label(self) -> &'static str {
        match self {
            StepId::Preflight => "preflight",
            StepId::Pair => "wallet pairing",
            StepId::Approve => "approve",
            StepId::Deposit => "deposit request",
            StepId::EvmConfirm => "EVM confirmation",
            StepId::Anchor => "block anchor on Acki Nacki",
            StepId::Prove => "proof",
            StepId::Finalize => "finalizeDeposit",
            StepId::Credit => "credit",
        }
    }
}

/// Where a step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Not started.
    Pending,
    /// In progress.
    Running,
    /// Finished successfully.
    Done,
    /// Ended in an error.
    Failed,
    /// Not needed on this run.
    Skipped,
}

/// The sink a deposit run reports its progress to.
pub trait Ui: Send + Sync {
    /// A step changed state; `detail` may be empty.
    fn step(&self, step: StepId, state: StepState, detail: &str);
    /// A free-form progress line.
    fn status(&self, text: &str);
    /// A transient error that is being retried.
    fn retry(&self, what: &str, attempt: u32, last_error: &str);
    /// Something the person should notice but that does not stop the run.
    fn warn(&self, text: &str);
    /// The pairing URI and its decoded fields, for the wallet to scan.
    fn qr(&self, uri: &str, decoded: &[(String, String)]);
    /// The operation id, once reserved.
    fn op_id(&self, op_id: &str);
    /// `false` when no answer can be had (`--non-interactive` without `--yes`).
    fn confirm(&self, prompt: &str) -> bool;
}

/// One call recorded by [`RecordingUi`].
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    /// A [`Ui::step`] call.
    Step(StepId, StepState, String),
    /// A [`Ui::status`] call.
    Status(String),
    /// A [`Ui::retry`] call.
    Retry(String, u32, String),
    /// A [`Ui::warn`] call.
    Warn(String),
    /// A [`Ui::qr`] call, the URI only.
    Qr(String),
    /// A [`Ui::op_id`] call.
    OpId(String),
    /// A [`Ui::confirm`] call, the prompt only.
    Confirm(String),
}

/// A [`Ui`] that keeps every call and answers `confirm` with a fixed value.
#[cfg(test)]
pub struct RecordingUi {
    events: std::sync::Mutex<Vec<UiEvent>>,
    answer: bool,
}

#[cfg(test)]
impl RecordingUi {
    /// A recorder whose `confirm` returns `answer`.
    pub fn new(answer: bool) -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
            answer,
        }
    }

    /// Every call so far, in order.
    pub fn events(&self) -> Vec<UiEvent> {
        self.events.lock().unwrap().clone()
    }

    /// The texts of the `status` calls, in order.
    pub fn statuses(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| {
                if let UiEvent::Status(s) = e {
                    Some(s)
                } else {
                    None
                }
            })
            .collect()
    }

    /// The texts of the `warn` calls, in order.
    pub fn warnings(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| {
                if let UiEvent::Warn(s) = e {
                    Some(s)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Appends one event.
    fn push(&self, e: UiEvent) {
        self.events.lock().unwrap().push(e);
    }
}

#[cfg(test)]
impl Ui for RecordingUi {
    fn step(&self, step: StepId, state: StepState, detail: &str) {
        self.push(UiEvent::Step(step, state, detail.into()));
    }
    fn status(&self, text: &str) {
        self.push(UiEvent::Status(text.into()));
    }
    fn retry(&self, what: &str, attempt: u32, last_error: &str) {
        self.push(UiEvent::Retry(what.into(), attempt, last_error.into()));
    }
    fn warn(&self, text: &str) {
        self.push(UiEvent::Warn(text.into()));
    }
    fn qr(&self, uri: &str, _decoded: &[(String, String)]) {
        self.push(UiEvent::Qr(uri.into()));
    }
    fn op_id(&self, op_id: &str) {
        self.push(UiEvent::OpId(op_id.into()));
    }
    fn confirm(&self, prompt: &str) -> bool {
        self.push(UiEvent::Confirm(prompt.into()));
        self.answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nine_steps_in_pipeline_order() {
        assert_eq!(StepId::ALL.len(), 9);
        assert_eq!(StepId::ALL[0], StepId::Preflight);
        assert_eq!(StepId::ALL[8], StepId::Credit);
        assert_eq!(
            serde_json::to_value(StepId::EvmConfirm).unwrap(),
            "evm_confirm"
        );
    }

    #[test]
    fn the_recorder_keeps_order() {
        let ui = RecordingUi::new(true);
        ui.step(StepId::Anchor, StepState::Running, "");
        ui.status("waiting for the bridge owner");
        ui.warn("anchor withdrawn");
        assert_eq!(ui.statuses(), vec![
            "waiting for the bridge owner".to_string()
        ]);
        assert_eq!(ui.warnings(), vec!["anchor withdrawn".to_string()]);
        assert!(ui.confirm("go?"));
    }
}
