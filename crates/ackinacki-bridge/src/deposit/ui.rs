//! What a deposit run tells the person watching it. Every step reports
//! through [`Ui`]; [`pick`] chooses the renderer once: NDJSON events on
//! stdout under `--json`, a redrawn step board when stderr is a terminal,
//! append-only lines otherwise. A human run prints its QR codes and the
//! final summary on stdout and everything else on stderr.
//!
//! Nothing a renderer prints carries a URL's path, query or userinfo: RPC
//! providers put their API keys there, and HTTP clients quote the whole
//! URL in their errors. See [`redact`].

use std::{
    borrow::Cow,
    cell::Cell,
    collections::HashMap,
    io::{IsTerminal as _, Write},
    sync::{Arc, Mutex, MutexGuard, RwLock, Weak},
    time::{Duration, Instant},
};

use async_trait::async_trait;

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
#[async_trait]
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
    /// `false` when no answer can be had (`--non-interactive` without
    /// `--yes`). Dropping the future ends a wait for the answer at once, as
    /// a signal that drops the run does.
    async fn confirm(&self, prompt: &str) -> bool;
}

/// The checklist glyph of a step state.
pub fn glyph(s: StepState) -> char {
    match s {
        StepState::Done => '✔',
        StepState::Running => '◐',
        StepState::Pending => '·',
        StepState::Failed => '✖',
        StepState::Skipped => '⊘',
    }
}

/// A duration as a timer shows it: `42s`, `3m07s`, `1h04m`.
fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    }
}

// -- secrets in printed text ------------------------------------------------

/// What stands where a secret part of a configured URL was.
const HIDDEN: &str = "<hidden>";

/// The shortest configured URL part, or registered secret value, hidden
/// where it appears on its own.
const MIN_SECRET_LEN: usize = 8;

/// Takes the secrets out of text on its way to the screen.
///
/// Every `scheme://…` URL in the text is cut to `scheme://host[:port]`,
/// however it is spelled: an HTTP client quotes the URL it normalised
/// (host in lower case, a `/` added), not the one configured. The secret
/// parts of the configured URLs — userinfo, path, query, fragment and their
/// pieces — are hidden wherever they appear on their own, as in a `Debug`
/// print of a parsed URL (`path: "/v2/<key>"`). A part is hidden on its own
/// only when it is at least [`MIN_SECRET_LEN`] characters long and mixes
/// letters and digits, as keys do: a bare word (`v2`, `sepolia`) or number
/// (a chain id) stays readable.
#[derive(Debug, Clone, Default)]
struct Redactor {
    /// Secret parts of the configured URLs, longest first.
    parts: Vec<String>,
}

impl Redactor {
    /// A redactor for the configured `urls`.
    #[cfg(test)]
    fn new<'a>(urls: impl IntoIterator<Item = &'a str>) -> Self {
        let mut r = Self::default();
        r.add(urls);
        r
    }

    /// Adds the secret parts of `urls`.
    fn add<'a>(&mut self, urls: impl IntoIterator<Item = &'a str>) {
        self.parts
            .extend(urls.into_iter().flat_map(secret_parts).filter(|p| {
                p.chars().count() >= MIN_SECRET_LEN
                    && p.contains(|c: char| c.is_ascii_digit())
                    && p.contains(|c: char| c.is_ascii_alphabetic())
            }));
        self.order();
    }

    /// Adds `values` as they are: secrets that are not URLs, such as a
    /// WalletConnect project id.
    fn add_values<'a>(&mut self, values: impl IntoIterator<Item = &'a str>) {
        self.parts.extend(
            values
                .into_iter()
                .filter(|v| v.chars().count() >= MIN_SECRET_LEN)
                .map(str::to_string),
        );
        self.order();
    }

    /// Longest first, so a whole query goes before a value inside it; each
    /// part once.
    fn order(&mut self) {
        self.parts
            .sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        self.parts.dedup();
    }

    /// `text` with every URL cut to its origin and every secret part hidden.
    fn apply(&self, text: &str) -> String {
        let mut s = cut_urls(text);
        for p in &self.parts {
            if s.contains(p.as_str()) {
                s = s.replace(p.as_str(), HIDDEN);
            }
        }
        s
    }
}

/// The parts of `url` besides its scheme and host, as typed and as a URL
/// parser renders them.
fn secret_parts(url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    // Userinfo ends at an `@` inside the authority; an `@` further on is
    // part of the path or the query.
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let after_userinfo = match authority.rfind('@') {
        Some(at) => {
            let userinfo = &rest[..at];
            out.push(userinfo.to_string());
            out.extend(userinfo.split(':').map(str::to_string));
            &rest[at + 1..]
        },
        None => rest,
    };
    if let Some(at) = after_userinfo.find(['/', '?', '#']) {
        let tail = &after_userinfo[at..];
        out.push(tail.trim_start_matches('/').to_string());
        for piece in tail.split(['/', '?', '&', '#', '@']) {
            out.push(piece.to_string());
            if let Some((_, value)) = piece.split_once('=') {
                out.push(value.to_string());
            }
        }
    }
    if let Ok(u) = url::Url::parse(url) {
        out.push(u.username().to_string());
        out.extend(u.password().map(str::to_string));
        out.push(u.path().trim_start_matches('/').to_string());
        out.extend(u.path_segments().into_iter().flatten().map(str::to_string));
        if let Some(q) = u.query() {
            out.push(q.to_string());
            out.extend(q.split('&').map(str::to_string));
        }
        out.extend(u.query_pairs().map(|(_, v)| v.into_owned()));
        out.extend(u.fragment().map(str::to_string));
    }
    out
}

/// `text` with every `scheme://…` URL cut to `scheme://host[:port]`. A URL
/// runs to the next space, quote or brace; the punctuation that ends a
/// sentence or a bracket around it (`.`, `,`, `)`, …) is kept after the cut.
/// Userinfo is what precedes an `@` before the first `/`, `?` or `#`; an
/// `@` after them belongs to the path or query and goes with it.
fn cut_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        // A scheme starts with a letter and goes on with letters, digits,
        // `+`, `-` or `.`.
        let head = &rest[..i];
        let run = head
            .char_indices()
            .rev()
            .take_while(|&(_, c)| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            .last()
            .map_or(i, |(j, _)| j);
        let has_scheme = head[run..].contains(|c: char| c.is_ascii_alphabetic());
        out.push_str(&rest[..i + 3]);
        let body = &rest[i + 3..];
        if !has_scheme {
            rest = body;
            continue;
        }
        let end = body
            .find(|c: char| {
                c.is_whitespace()
                    || c.is_control()
                    || matches!(
                        c,
                        '"' | '\'' | '`' | '<' | '>' | '\\' | '{' | '}' | '|' | '^'
                    )
            })
            .unwrap_or(body.len());
        let url = &body[..end];
        let kept = url.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
        let authority = &kept[..kept.find(['/', '?', '#']).unwrap_or(kept.len())];
        out.push_str(&authority[authority.rfind('@').map_or(0, |at| at + 1)..]);
        out.push_str(&url[kept.len()..]);
        rest = &body[end..];
    }
    out.push_str(rest);
    out
}

/// What this process's deposit run hides, once `main` has seen one. `None`
/// in any other run, whose error printout stays as it is.
static DEPOSIT_RUN: RwLock<Option<Redactor>> = RwLock::new(None);

/// Marks this process as a deposit run and adds the secret parts of the
/// configured `urls` to what its output hides. From here on the error
/// printout is redacted too, not only the progress renderers.
pub fn hide_url_secrets<'a>(urls: impl IntoIterator<Item = &'a str>) {
    DEPOSIT_RUN
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(Redactor::default)
        .add(urls);
}

/// Adds `values`, secrets that are not URLs (a WalletConnect project id),
/// to what this process's output hides wherever they appear, and marks the
/// process as a deposit run as [`hide_url_secrets`] does.
pub fn hide_secret_values<'a>(values: impl IntoIterator<Item = &'a str>) {
    DEPOSIT_RUN
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(Redactor::default)
        .add_values(values);
}

/// `text` as a deposit renderer prints it: every URL cut to
/// `scheme://host[:port]`, and the secret parts of the configured URLs
/// hidden wherever they appear. Also for a reason written into an
/// operation record that is printed later.
pub fn redact(text: &str) -> String {
    match &*DEPOSIT_RUN.read().unwrap_or_else(|p| p.into_inner()) {
        Some(r) => r.apply(text),
        None => cut_urls(text),
    }
}

/// [`redact`] in a deposit run; in any other, `text` as it is.
pub fn redact_in_deposit_run(text: &str) -> Cow<'_, str> {
    match &*DEPOSIT_RUN.read().unwrap_or_else(|p| p.into_inner()) {
        Some(r) => Cow::Owned(r.apply(text)),
        None => Cow::Borrowed(text),
    }
}

/// `s` safe for a terminal: control characters other than a line break are
/// escaped, so a node's error text cannot move the cursor or repaint the
/// screen.
fn printable(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() && c != '\n' {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out
}

/// `s` redacted and printable: what every human line is made of.
fn shown(s: &str) -> String {
    printable(&redact(s))
}

/// `s` on one line, for a board row.
fn one_line(s: &str) -> String {
    s.lines().collect::<Vec<_>>().join(" / ")
}

// -- shared by the human renderers ------------------------------------------

/// Writes `s` whole and flushes it.
fn put(out: &mut dyn Write, s: &str) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "progress output that cannot be written is not a deposit failure"
    )]
    let _ = out.write_all(s.as_bytes()).and_then(|()| out.flush());
}

/// How a human renderer shows a QR code and answers a question.
#[derive(Debug, Clone, Copy, Default)]
struct Settings {
    /// `--uri-only`: the URI in words, never a picture.
    uri_only: bool,
    /// `--qr-invert`: dark and light swapped, for a light-on-dark terminal.
    invert: bool,
    /// `--yes`: every question is answered yes.
    yes: bool,
    /// `--non-interactive`: a question `--yes` does not answer is a no.
    non_interactive: bool,
}

/// The answer `--yes` or `--non-interactive` gives to `prompt` without
/// asking, and the line that says so.
fn answered(prompt: &str, s: &Settings) -> Option<(bool, String)> {
    if s.yes {
        return Some((true, format!("{prompt} [y/N] yes (--yes)")));
    }
    if s.non_interactive {
        return Some((false, format!("{prompt} [y/N] no (--non-interactive)")));
    }
    None
}

/// One line of this process's stdin; `None` at its end or on an error.
fn stdin_line() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(n) if n > 0 => Some(line),
        _ => None,
    }
}

/// The answer to a question: one line of stdin, read on a thread of its
/// own; `None` when no line came. Dropping the future ends the wait at
/// once, and the locks of the run that dropped it go with the run. The
/// thread stays blocked in its read, but it is no task of the runtime:
/// neither the runtime's shutdown nor the process's exit waits for it. A
/// blocking-pool read would hold both until a line came.
async fn stdin_answer() -> Option<String> {
    let (sent, got) = tokio::sync::oneshot::channel();
    let reader = std::thread::Builder::new()
        .name("deposit-answer".into())
        .spawn(move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "a question given up on no longer listens"
            )]
            let _ = sent.send(stdin_line());
        });
    if let Err(e) = reader {
        tracing::debug!("the answer could not be read: {e}");
        return None;
    }
    got.await.ok().flatten()
}

/// Whether an answer says yes: `y` or `yes`, in any case; anything else,
/// no answer included, is a no.
fn says_yes(answer: Option<&str>) -> bool {
    answer.is_some_and(|l| matches!(l.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Where a human run prints its QR codes: stdout, with the picture only
/// when stdout is a terminal it can be scanned off.
struct Codes {
    /// The stream.
    out: Box<dyn Write + Send>,
    /// The stream is a terminal.
    terminal: bool,
}

impl Codes {
    /// This process's stdout.
    #[cfg(test)]
    fn stdout() -> Self {
        Self {
            terminal: std::io::stdout().is_terminal(),
            out: Box::new(std::io::stdout()),
        }
    }
}

/// The words printed with a QR code: its decoded fields, then the URI. The
/// URI is printed whole: it is what the wallet needs.
fn qr_text(uri: &str, decoded: &[(String, String)]) -> String {
    let mut s = String::new();
    for (k, v) in decoded {
        s.push_str(&format!("  {}: {}\n", shown(k), shown(v)));
    }
    s.push_str(&format!("  URI: {}\n", printable(uri)));
    s
}

/// The code of `uri` as terminal text, unless `--uri-only`.
fn picture(uri: &str, s: &Settings) -> Option<String> {
    if s.uri_only {
        return None;
    }
    crate::deposit::qr::render_terminal(uri, s.invert).ok()
}

// -- plain lines ------------------------------------------------------------

/// Append-only lines without escape codes, for a stderr that is not a
/// terminal: a log file, a pipe, a CI job. A repeated status is printed
/// once.
pub struct PlainLines {
    /// The log stream and what it remembers.
    log: Mutex<Log>,
    /// Where the QR codes go.
    codes: Mutex<Codes>,
    /// The flags that shape codes and answers.
    settings: Settings,
}

/// A plain-lines log and what it remembers.
struct Log {
    /// The stream.
    out: Box<dyn Write + Send>,
    /// The last status printed.
    last_status: String,
    /// When each running step started.
    started: HashMap<StepId, Instant>,
    /// A question waits for its answer on the last line.
    asking: bool,
}

impl PlainLines {
    /// Lines on `out`, QR codes on stdout.
    #[cfg(test)]
    pub fn new(out: Box<dyn Write + Send>, yes: bool, non_interactive: bool) -> Self {
        Self::build(out, Codes::stdout(), Settings {
            yes,
            non_interactive,
            ..Settings::default()
        })
    }

    /// Lines on `out`, QR codes on `codes`.
    fn build(out: Box<dyn Write + Send>, codes: Codes, settings: Settings) -> Self {
        Self {
            log: Mutex::new(Log {
                out,
                last_status: String::new(),
                started: HashMap::new(),
                asking: false,
            }),
            codes: Mutex::new(codes),
            settings,
        }
    }

    /// The log, even after a panic elsewhere.
    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Prints one line.
    fn line(&self, s: &str) {
        put(self.log().out.as_mut(), &format!("{s}\n"));
    }
}

impl Drop for PlainLines {
    /// A run dropped at a question ends its line: what is printed next
    /// starts on its own.
    fn drop(&mut self) {
        let mut log = self.log();
        if log.asking {
            put(log.out.as_mut(), "\n");
        }
    }
}

#[async_trait]
impl Ui for PlainLines {
    fn step(&self, step: StepId, state: StepState, detail: &str) {
        let mut log = self.log();
        let took = match state {
            StepState::Running => {
                log.started.insert(step, Instant::now());
                String::new()
            },
            _ => log
                .started
                .remove(&step)
                .map(|t| format!(" ({})", elapsed(t.elapsed())))
                .unwrap_or_default(),
        };
        // A step's statuses are its own: the next step may repeat one.
        log.last_status.clear();
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!(": {}", shown(detail))
        };
        let line = format!("{} {}{took}{detail}\n", glyph(state), step.label());
        put(log.out.as_mut(), &line);
    }

    fn status(&self, text: &str) {
        let text = shown(text);
        let mut log = self.log();
        if log.last_status != text {
            put(log.out.as_mut(), &format!("  … {text}\n"));
            log.last_status = text;
        }
    }

    fn retry(&self, what: &str, attempt: u32, last_error: &str) {
        self.line(&format!(
            "ERROR {} (attempt {attempt}): {}",
            shown(what),
            shown(last_error)
        ));
    }

    fn warn(&self, text: &str) {
        self.line(&format!("WARN {}", shown(text)));
    }

    fn qr(&self, uri: &str, decoded: &[(String, String)]) {
        {
            let mut codes = self.codes.lock().unwrap_or_else(|p| p.into_inner());
            let mut block = String::new();
            if codes.terminal {
                if let Some(p) = picture(uri, &self.settings) {
                    block.push_str(&p);
                    block.push('\n');
                }
            }
            block.push_str(&qr_text(uri, decoded));
            put(codes.out.as_mut(), &block);
        }
        self.line(&format!("URI: {}", printable(uri)));
    }

    fn op_id(&self, op_id: &str) {
        let op = shown(op_id);
        self.line(&format!(
            "operation {op} (keep it: `--resume {op}` continues this deposit)"
        ));
    }

    async fn confirm(&self, prompt: &str) -> bool {
        let prompt = shown(prompt);
        if let Some((yes, line)) = answered(&prompt, &self.settings) {
            self.line(&line);
            return yes;
        }
        {
            let mut log = self.log();
            log.asking = true;
            put(log.out.as_mut(), &format!("{prompt} [y/N] "));
        }
        let answer = stdin_answer().await;
        let mut log = self.log();
        log.asking = false;
        if answer.is_none() {
            put(log.out.as_mut(), "\n");
        }
        says_yes(answer.as_deref())
    }
}

// -- NDJSON -----------------------------------------------------------------

/// NDJSON events on stdout for `--json`, one per line, each carrying the
/// operation id once there is one. A JSON consumer cannot answer a
/// question, so `--yes` is the answer.
pub struct JsonEvents {
    /// The stream and the operation id.
    out: Mutex<JsonOut>,
    /// `--yes`.
    yes: bool,
}

/// An NDJSON stream and the operation its events belong to.
struct JsonOut {
    /// The stream.
    out: Box<dyn Write + Send>,
    /// The operation id, once reserved.
    op_id: Option<String>,
}

impl JsonEvents {
    /// Events on `out`; `yes` answers every question.
    pub fn new(out: Box<dyn Write + Send>, yes: bool) -> Self {
        Self {
            out: Mutex::new(JsonOut {
                out,
                op_id: None,
            }),
            yes,
        }
    }

    /// Writes one event, with the operation id when there is one.
    fn emit(&self, mut v: serde_json::Value) {
        let mut o = self.out.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(op) = &o.op_id {
            v["op_id"] = serde_json::json!(op);
        }
        let line = format!("{v}\n");
        put(o.out.as_mut(), &line);
    }
}

#[async_trait]
impl Ui for JsonEvents {
    fn step(&self, step: StepId, state: StepState, detail: &str) {
        self.emit(serde_json::json!({
            "event": "step", "step": step, "state": state, "detail": redact(detail),
        }));
    }

    fn status(&self, text: &str) {
        self.emit(serde_json::json!({ "event": "status", "text": redact(text) }));
    }

    fn retry(&self, what: &str, attempt: u32, last_error: &str) {
        self.emit(serde_json::json!({
            "event": "retry", "what": redact(what), "attempt": attempt, "error": redact(last_error),
        }));
    }

    fn warn(&self, text: &str) {
        self.emit(serde_json::json!({ "event": "warn", "text": redact(text) }));
    }

    fn qr(&self, uri: &str, decoded: &[(String, String)]) {
        let d: serde_json::Map<String, serde_json::Value> = decoded
            .iter()
            .map(|(k, v)| (redact(k), serde_json::json!(redact(v))))
            .collect();
        self.emit(serde_json::json!({ "event": "qr", "uri": uri, "decoded": d }));
    }

    fn op_id(&self, op_id: &str) {
        self.out.lock().unwrap_or_else(|p| p.into_inner()).op_id = Some(op_id.to_string());
        self.emit(serde_json::json!({ "event": "op_id" }));
    }

    async fn confirm(&self, prompt: &str) -> bool {
        self.emit(serde_json::json!({
            "event": "confirm", "prompt": redact(prompt), "answer": self.yes,
        }));
        self.yes
    }
}

// -- terminal board ---------------------------------------------------------

/// How often the board redraws itself while its timers move.
const TICK: Duration = Duration::from_secs(1);

/// The frames of the status line's spinner.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The nine steps redrawn in place under a status line, for a stderr that is
/// a terminal. Warnings, errors, questions and QR codes print above the
/// board and stay; each step keeps its timer and its detail after it ends.
/// While a step runs, the board redraws itself every second, so its timer,
/// the spinner and the age of the last error keep moving between events.
pub struct TtyBoard {
    /// The board, shared with the thread that keeps it ticking.
    board: Arc<Mutex<Board>>,
}

/// A board's state and its streams, under one lock: a frame is built and
/// written whole, and a question keeps the board off the screen until it
/// is answered.
struct Board {
    /// The terminal the board is drawn on.
    out: Box<dyn Write + Send>,
    /// Where the QR codes go.
    codes: Codes,
    /// The flags that shape codes and answers.
    settings: Settings,
    /// One row per step, in [`StepId::ALL`] order.
    rows: Vec<Row>,
    /// The status line's text.
    status: String,
    /// When the status text last changed.
    status_since: Instant,
    /// The attempt number and time of the last retried error, until the
    /// step changes.
    retry: Option<(u32, Instant)>,
    /// Lines of the last frame on the screen, above the cursor.
    drawn: usize,
    /// Frames drawn, for the spinner.
    spin: usize,
    /// The terminal's width in columns, when it can be read.
    width: fn() -> Option<usize>,
    /// The board is gone: its clock stops.
    closed: bool,
    /// A question waits for its answer where the board stood.
    asking: bool,
}

/// One step's row on the board.
struct Row {
    /// Where the step stands.
    state: StepState,
    /// What the step last said about itself.
    detail: String,
    /// When the step started running.
    started: Option<Instant>,
    /// How long the step ran, once it ended.
    took: Option<Duration>,
}

impl Row {
    /// A step that has not started.
    fn pending() -> Self {
        Self {
            state: StepState::Pending,
            detail: String::new(),
            started: None,
            took: None,
        }
    }
}

/// The width of the longest step label, so that the timers line up.
fn label_width() -> usize {
    StepId::ALL
        .iter()
        .map(|s| s.label().chars().count())
        .max()
        .unwrap_or(0)
}

/// The width in columns of the terminal on stderr, when stderr is one.
fn stderr_width() -> Option<usize> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one `winsize` into the struct it is given
    // and nothing else.
    let rc = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut ws) };
    (rc == 0 && ws.ws_col > 0).then_some(usize::from(ws.ws_col))
}

impl Board {
    /// A step runs or an error is being retried: the timers move.
    fn moving(&self) -> bool {
        self.retry.is_some() || self.rows.iter().any(|r| r.state == StepState::Running)
    }

    /// `s` cut to the terminal's width: a line that wrapped would take two
    /// screen lines, and the next redraw would start one line too low.
    fn fit(&self, s: String) -> String {
        match (self.width)() {
            Some(w) if s.chars().count() >= w => {
                let mut cut: String = s.chars().take(w.saturating_sub(2)).collect();
                cut.push('…');
                cut
            },
            _ => s,
        }
    }

    /// Row `i` as text: glyph, label, timer, detail.
    fn row(&self, i: usize) -> String {
        let r = &self.rows[i];
        let timer = match (r.state, r.started, r.took) {
            (StepState::Running, Some(t), _) => elapsed(t.elapsed()),
            (_, _, Some(d)) => elapsed(d),
            _ => String::new(),
        };
        let line = format!(
            "{} {:<w$} {timer:>6}  {}",
            glyph(r.state),
            StepId::ALL[i].label(),
            r.detail,
            w = label_width()
        );
        line.trim_end().to_string()
    }

    /// The status line: spinner, status and how long it has held, then the
    /// attempt number and the age of the last error.
    fn status_line(&self) -> String {
        let mut s = String::new();
        if !self.status.is_empty() {
            s = format!("{} · {}", self.status, elapsed(self.status_since.elapsed()));
        }
        if let Some((n, at)) = self.retry {
            if !s.is_empty() {
                s.push_str(" · ");
            }
            s.push_str(&format!(
                "attempt {n}, last error {} ago",
                elapsed(at.elapsed())
            ));
        }
        if s.is_empty() || self.closed || !self.moving() {
            return s;
        }
        format!("{} {s}", SPINNER[self.spin % SPINNER.len()])
    }

    /// Draws the board over its last frame, with `above` printed once
    /// above it, where it stays.
    fn draw(&mut self, above: &str) {
        let mut s = String::new();
        if self.drawn > 0 {
            s.push_str(&format!("\x1b[{}A", self.drawn));
        }
        for l in above.lines() {
            s.push_str("\x1b[2K");
            s.push_str(l);
            s.push('\n');
        }
        for i in 0..self.rows.len() {
            let row = self.fit(self.row(i));
            s.push_str("\x1b[2K");
            s.push_str(&row);
            s.push('\n');
        }
        let status = self.fit(self.status_line());
        s.push_str("\x1b[2K");
        s.push_str(&status);
        s.push('\n');
        self.drawn = self.rows.len() + 1;
        self.spin = self.spin.wrapping_add(1);
        put(self.out.as_mut(), &s);
    }

    /// Takes the board off the screen, so that what is printed next lands
    /// where it stood.
    fn erase(&mut self) {
        if self.drawn > 0 {
            let s = format!("\x1b[{}A\x1b[J", self.drawn);
            put(self.out.as_mut(), &s);
            self.drawn = 0;
        }
    }

    /// Prints the log line `line` above the board, as a warning is, when
    /// the board is on the screen; `false` when it is not — before its
    /// first frame, while a question stands where it was, once it is gone.
    fn log(&mut self, line: &[u8]) -> bool {
        if self.closed || self.drawn == 0 {
            return false;
        }
        self.draw(&String::from_utf8_lossy(line));
        true
    }
}

thread_local! {
    /// This thread holds a board's lock.
    static IN_BOARD: Cell<bool> = const { Cell::new(false) };
}

/// A board's lock, held by this thread. A log line this thread writes
/// meanwhile does not wait for it: it goes out as if there were no board.
struct Held<'a>(MutexGuard<'a, Board>);

/// Takes the board's lock, even after a panic elsewhere.
fn hold(board: &Mutex<Board>) -> Held<'_> {
    let guard = board.lock().unwrap_or_else(|p| p.into_inner());
    IN_BOARD.set(true);
    Held(guard)
}

impl std::ops::Deref for Held<'_> {
    type Target = Board;

    fn deref(&self) -> &Board {
        &self.0
    }
}

impl std::ops::DerefMut for Held<'_> {
    fn deref_mut(&mut self) -> &mut Board {
        &mut self.0
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        IN_BOARD.set(false);
    }
}

/// The board on this process's terminal, once a run has one: log lines go
/// above it while it is on the screen. Written under the cursor, a line
/// would be painted over by the next frame and leave a stale row above it.
static LOG_BOARD: Mutex<Option<Weak<Mutex<Board>>>> = Mutex::new(None);

/// Redraws the board every `every` while its timers move; returns once the
/// board is gone.
fn keep_ticking(board: &Weak<Mutex<Board>>, every: Duration) {
    loop {
        std::thread::sleep(every);
        let Some(board) = board.upgrade() else {
            return;
        };
        let mut b = hold(&board);
        if b.closed {
            return;
        }
        if b.drawn > 0 && b.moving() {
            b.draw("");
        }
    }
}

impl TtyBoard {
    /// A board on `out`, QR codes on `codes`, rows cut to `width`,
    /// redrawn every `tick` while a step runs (never without one).
    fn build(
        out: Box<dyn Write + Send>,
        codes: Codes,
        settings: Settings,
        width: fn() -> Option<usize>,
        tick: Option<Duration>,
    ) -> Self {
        let board = Arc::new(Mutex::new(Board {
            out,
            codes,
            settings,
            rows: StepId::ALL.iter().map(|_| Row::pending()).collect(),
            status: String::new(),
            status_since: Instant::now(),
            retry: None,
            drawn: 0,
            spin: 0,
            width,
            closed: false,
            asking: false,
        }));
        if let Some(every) = tick {
            let weak = Arc::downgrade(&board);
            let started = std::thread::Builder::new()
                .name("deposit-board".into())
                .spawn(move || keep_ticking(&weak, every));
            if let Err(e) = started {
                // The board still redraws on every event.
                tracing::debug!("the step board's clock did not start: {e}");
            }
        }
        Self {
            board,
        }
    }

    /// The board, even after a panic elsewhere.
    fn lock(&self) -> Held<'_> {
        hold(&self.board)
    }
}

/// Prints `line`, one complete log line as the log writes it, above the
/// board of this process's terminal while it is on the screen. `false`
/// when no board is, or when this thread is drawing it, and the caller
/// writes the line as it would without a board.
pub fn log_above_board(line: &[u8]) -> bool {
    if IN_BOARD.get() {
        return false;
    }
    let board = LOG_BOARD
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(Weak::upgrade);
    board.is_some_and(|b| hold(&b).log(line))
}

impl TtyBoard {
    /// Log lines of this process go above this board while it is on the
    /// screen.
    fn take_log_lines(&self) {
        *LOG_BOARD.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::downgrade(&self.board));
    }

    /// Prints the log line `line` above the board when it is on the
    /// screen; `false` when it is not.
    #[cfg(test)]
    fn log_line(&self, line: &[u8]) -> bool {
        self.lock().log(line)
    }
}

impl Drop for TtyBoard {
    fn drop(&mut self) {
        let mut b = self.lock();
        b.closed = true;
        // A run dropped at a question ends its line: what is printed next
        // starts on its own.
        if b.asking {
            put(b.out.as_mut(), "\n");
        }
        // The last frame stays on the screen, without the spinner.
        if b.drawn > 0 {
            b.draw("");
        }
    }
}

#[async_trait]
impl Ui for TtyBoard {
    fn step(&self, step: StepId, state: StepState, detail: &str) {
        let detail = one_line(&shown(detail));
        let mut b = self.lock();
        let i = StepId::ALL.iter().position(|s| *s == step).unwrap_or(0);
        let row = &mut b.rows[i];
        if state == StepState::Running {
            row.started = Some(Instant::now());
            row.took = None;
        } else if let Some(t) = row.started.take() {
            row.took = Some(t.elapsed());
        }
        row.state = state;
        if !detail.is_empty() || state == StepState::Running {
            row.detail = detail;
        }
        // A new step state is a new wait: the old status and retry go.
        b.status.clear();
        b.retry = None;
        b.draw("");
    }

    fn status(&self, text: &str) {
        let text = one_line(&shown(text));
        let mut b = self.lock();
        if b.status != text {
            b.status = text;
            b.status_since = Instant::now();
        }
        b.draw("");
    }

    fn retry(&self, what: &str, attempt: u32, last_error: &str) {
        let line = format!(
            "ERROR {} (attempt {attempt}): {}",
            shown(what),
            shown(last_error)
        );
        let mut b = self.lock();
        b.retry = Some((attempt, Instant::now()));
        b.draw(&line);
    }

    fn warn(&self, text: &str) {
        let line = format!("WARN {}", shown(text));
        self.lock().draw(&line);
    }

    fn qr(&self, uri: &str, decoded: &[(String, String)]) {
        let text = qr_text(uri, decoded);
        let mut b = self.lock();
        let picture = picture(uri, &b.settings);
        // Off the screen first: the code must not land under the cursor's
        // way back up, or the next frame would draw over it.
        b.erase();
        if b.codes.terminal {
            let block = match picture {
                Some(p) => format!("{p}\n{text}"),
                None => text,
            };
            put(b.codes.out.as_mut(), &block);
            b.draw("");
        } else {
            // stdout is a file or a pipe: it gets the words, and the code is
            // drawn here, where it can be scanned.
            put(b.codes.out.as_mut(), &text);
            let uri_line = format!("URI: {}", printable(uri));
            let above = match picture {
                Some(p) => format!("{p}\n{uri_line}"),
                None => uri_line,
            };
            b.draw(&above);
        }
    }

    fn op_id(&self, op_id: &str) {
        let op = shown(op_id);
        let line = format!("operation {op} (keep it: `--resume {op}` continues this deposit)");
        self.lock().draw(&line);
    }

    async fn confirm(&self, prompt: &str) -> bool {
        let prompt = shown(prompt);
        {
            let mut b = self.lock();
            if let Some((yes, line)) = answered(&prompt, &b.settings) {
                b.draw(&line);
                return yes;
            }
            // Asked where the board stood, which the clock leaves alone
            // while it is off the screen; the board comes back under the
            // answer.
            b.erase();
            b.asking = true;
            put(b.out.as_mut(), &format!("{prompt} [y/N] "));
        }
        let answer = stdin_answer().await;
        let mut b = self.lock();
        b.asking = false;
        if answer.is_none() {
            put(b.out.as_mut(), "\n");
        }
        b.draw("");
        says_yes(answer.as_deref())
    }
}

// -- choosing ---------------------------------------------------------------

/// The terminal streams [`pick`] chooses between, and what it needs to
/// know about them.
struct Terminal {
    /// Where NDJSON, QR codes and the summary go.
    stdout: Box<dyn Write + Send>,
    /// stdout is a terminal.
    stdout_tty: bool,
    /// Where the board or the lines go.
    stderr: Box<dyn Write + Send>,
    /// stderr is a terminal.
    stderr_tty: bool,
    /// The width of the terminal on stderr.
    width: fn() -> Option<usize>,
    /// How often the board redraws itself while a step runs.
    tick: Option<Duration>,
    /// The process's log lines are written to `stderr` too, and a board
    /// takes them.
    logs: bool,
}

impl Terminal {
    /// This process's stdout and stderr.
    fn process() -> Self {
        Self {
            stdout: Box::new(std::io::stdout()),
            stdout_tty: std::io::stdout().is_terminal(),
            stderr: Box::new(std::io::stderr()),
            stderr_tty: std::io::stderr().is_terminal(),
            width: stderr_width,
            tick: Some(TICK),
            logs: true,
        }
    }
}

/// The renderer for this run: NDJSON on stdout under `--json`, the step
/// board when stderr is a terminal, plain lines otherwise. `--uri-only` and
/// `--qr-invert` shape the QR codes a human run prints.
pub fn pick(g: &crate::deposit::args::GlobalFlags, uri_only: bool, qr_invert: bool) -> Arc<dyn Ui> {
    pick_from(g, uri_only, qr_invert, Terminal::process())
}

/// [`pick`] over the streams in `t`.
fn pick_from(
    g: &crate::deposit::args::GlobalFlags,
    uri_only: bool,
    qr_invert: bool,
    t: Terminal,
) -> Arc<dyn Ui> {
    if g.json {
        return Arc::new(JsonEvents::new(t.stdout, g.yes));
    }
    let codes = Codes {
        out: t.stdout,
        terminal: t.stdout_tty,
    };
    let settings = Settings {
        uri_only,
        invert: qr_invert,
        yes: g.yes,
        non_interactive: g.non_interactive,
    };
    if t.stderr_tty {
        let board = TtyBoard::build(t.stderr, codes, settings, t.width, t.tick);
        if t.logs {
            board.take_log_lines();
        }
        return Arc::new(board);
    }
    Arc::new(PlainLines::build(t.stderr, codes, settings))
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
#[async_trait]
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
    async fn confirm(&self, prompt: &str) -> bool {
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
        assert!(futures::executor::block_on(ui.confirm("go?")));
    }

    /// Everything written to a captured stream.
    type Captured = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    fn capture() -> (Captured, Box<dyn std::io::Write + Send>) {
        struct W(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for W {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (buf.clone(), Box::new(W(buf)))
    }

    /// What a capture holds, as text.
    fn text(buf: &Captured) -> String {
        String::from_utf8(buf.lock().unwrap().clone()).unwrap()
    }

    /// `s` without its ANSI control sequences: what the terminal shows.
    fn visible(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The cursor-up sequence that opens every frame after the first.
    fn frame_marker() -> String {
        format!("\x1b[{}A", StepId::ALL.len() + 1)
    }

    /// A board with its code sink captured, no width and no clock.
    fn board(settings: Settings, stdout_is_a_terminal: bool) -> (Captured, Captured, TtyBoard) {
        let (log, out) = capture();
        let (codes, sink) = capture();
        let ui = TtyBoard::build(
            out,
            Codes {
                out: sink,
                terminal: stdout_is_a_terminal,
            },
            settings,
            || None,
            None,
        );
        (log, codes, ui)
    }

    const PAIRING: &str = "wc:c9c71dfb61298046cee15333ff7bd4431d01ac59814cc2783e1e4ba57c033d13@2?\
                           relay-protocol=irn&\
                           symKey=0f77e74c4faf2feee58fc2c41be0d0f32d5bd32150bdb548ffa65beb2b1ca573&\
                           expiryTimestamp=1700000300";

    #[test]
    fn plain_lines_have_no_escape_codes_and_no_repeated_statuses() {
        let (buf, out) = capture();
        let ui = PlainLines::new(out, false, true);
        ui.step(StepId::Anchor, StepState::Running, "");
        ui.status("waiting for the bridge owner");
        ui.status("waiting for the bridge owner");
        ui.step(
            StepId::Anchor,
            StepState::Done,
            "anchored by the bridge owner",
        );
        let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(!s.contains('\x1b'));
        assert_eq!(s.matches("waiting for the bridge owner").count(), 1);
        assert!(s.contains("anchored by the bridge owner"));
    }

    #[test]
    fn json_events_carry_the_operation_and_the_uri_not_a_picture() {
        let (buf, out) = capture();
        let ui = JsonEvents::new(out, false);
        ui.op_id("01J9ZQ4X7T8V5N6M3K2P1R0S9A");
        ui.qr("wc:abc@2?relay-protocol=irn&symKey=00", &[]);
        ui.step(StepId::Pair, StepState::Done, "");
        let lines: Vec<serde_json::Value> = String::from_utf8(buf.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(lines
            .iter()
            .all(|l| l["op_id"] == "01J9ZQ4X7T8V5N6M3K2P1R0S9A"));
        let qr = lines.iter().find(|l| l["event"] == "qr").unwrap();
        assert_eq!(qr["uri"], "wc:abc@2?relay-protocol=irn&symKey=00");
        assert!(!qr.to_string().contains('▀'));
    }

    #[tokio::test]
    async fn json_with_yes_accepts_the_eip681_risk_and_without_yes_refuses_it() {
        use crate::deposit::wallet::{eip681::Eip681Wallet, Wallet, WalletError};
        let from = alloy_primitives::address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let (_b, out) = capture();
        let yes = JsonEvents::new(out, true);
        assert_eq!(
            Eip681Wallet::new(from, 11_155_111).connect(&yes).await,
            Ok(from)
        );
        let (_b, out) = capture();
        let no = JsonEvents::new(out, false);
        assert_eq!(
            Eip681Wallet::new(from, 11_155_111).connect(&no).await,
            Err(WalletError::Rejected)
        );
    }

    #[tokio::test]
    async fn the_human_renderers_take_yes_as_the_answer_to_the_eip681_risk() {
        use crate::deposit::wallet::{eip681::Eip681Wallet, Wallet, WalletError};
        let from = alloy_primitives::address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let connect = |ui: Box<dyn Ui>| async move {
            Eip681Wallet::new(from, 11_155_111)
                .connect(ui.as_ref())
                .await
        };
        let (_log, _codes, ui) = board(
            Settings {
                yes: true,
                ..Settings::default()
            },
            true,
        );
        assert_eq!(connect(Box::new(ui)).await, Ok(from));
        let (_b, out) = capture();
        assert_eq!(
            connect(Box::new(PlainLines::new(out, true, false))).await,
            Ok(from)
        );
        let (b, _codes, ui) = board(
            Settings {
                non_interactive: true,
                ..Settings::default()
            },
            true,
        );
        assert_eq!(connect(Box::new(ui)).await, Err(WalletError::Rejected));
        assert!(text(&b).contains("no (--non-interactive)"), "{}", text(&b));
        let (b, out) = capture();
        assert_eq!(
            connect(Box::new(PlainLines::new(out, false, true))).await,
            Err(WalletError::Rejected)
        );
        assert!(text(&b).contains("no (--non-interactive)"), "{}", text(&b));
    }

    #[tokio::test]
    async fn non_interactive_without_yes_never_waits() {
        let (_b, out) = capture();
        assert!(!PlainLines::new(out, false, true).confirm("go?").await);
        let (_b, out) = capture();
        assert!(PlainLines::new(out, true, true).confirm("go?").await);
    }

    /// Not a test on its own: the body of the process the tests below
    /// start with a piped stdin. Both human renderers ask a question. With
    /// ACKI_ASK_HELPER=unanswered nobody writes to that stdin, a timeout
    /// cuts each question short, as a signal does, and the runtime is
    /// dropped: the process must exit all the same. With `answered` the
    /// lines `y` and `no` come, one per question. Does nothing unless
    /// ACKI_ASK_HELPER is set.
    #[test]
    fn helper_ask_on_a_piped_stdin() {
        let Some(mode) = std::env::var_os("ACKI_ASK_HELPER") else {
            return;
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_log, out) = capture();
        let plain = PlainLines::new(out, false, false);
        let (_log, _codes, tty) = board(Settings::default(), true);
        let renderers = [&plain as &dyn Ui, &tty];
        if mode == "answered" {
            for (ui, yes) in renderers.into_iter().zip([true, false]) {
                assert_eq!(rt.block_on(ui.confirm("go?")), yes);
            }
            return;
        }
        for ui in renderers {
            let asked = rt.block_on(async {
                tokio::time::timeout(Duration::from_millis(200), ui.confirm("go?")).await
            });
            assert!(asked.is_err(), "nobody answered");
        }
        drop(rt);
    }

    /// Runs [`helper_ask_on_a_piped_stdin`] in `mode` with `input` written
    /// to its stdin, which stays open; how it exited within ten seconds.
    fn ask_in_a_process(mode: &str, input: &[u8]) -> Option<(std::process::ExitStatus, String)> {
        use std::process::{Command, Stdio};
        let _spawning = crate::test_forks::spawning();
        let mut cli = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "deposit::ui::tests::helper_ask_on_a_piped_stdin",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ACKI_ASK_HELPER", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = cli.stdin.take().unwrap();
        std::io::Write::write_all(&mut stdin, input).unwrap();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if let Some(status) = cli.try_wait().unwrap() {
                let mut err = String::new();
                std::io::Read::read_to_string(&mut cli.stderr.take().unwrap(), &mut err).unwrap();
                return Some((status, err));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cli.kill().unwrap();
        cli.wait().unwrap();
        None
    }

    #[test]
    fn a_question_nobody_answers_ends_with_the_run_that_asked_it() {
        // Ctrl-C drops the run's future while the EIP-681 question waits
        // for a line. The wait must end with it, and the process must exit
        // with the read still blocked, the deposit's locks let go.
        let (status, err) = ask_in_a_process("unanswered", b"")
            .expect("an unanswered question kept the process alive");
        assert!(status.success(), "{status}: {err}");
    }

    #[test]
    fn a_question_on_stdin_still_gets_its_answer() {
        let (status, err) =
            ask_in_a_process("answered", b"y\nno\n").expect("the answers were not read");
        assert!(status.success(), "{status}: {err}");
    }

    #[test]
    fn the_board_keeps_a_finished_steps_detail_and_puts_warnings_above() {
        let (buf, _codes, ui) = board(
            Settings {
                yes: true,
                ..Settings::default()
            },
            true,
        );
        ui.step(
            StepId::Preflight,
            StepState::Done,
            "bridge 1.6.0, anchors by the bridge owner",
        );
        ui.warn("anchor withdrawn");
        ui.step(StepId::Pair, StepState::Running, "");
        let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        // The last frame opens with the cursor going up over the board. The
        // marker is ASCII, so slicing at it stays on a char boundary.
        let last = &s[s.rfind(&frame_marker()).expect("a redrawn frame")..];
        assert!(last.contains("bridge 1.6.0, anchors by the bridge owner"));
        assert!(last.contains('◐'));
        assert!(
            !last.contains("anchor withdrawn"),
            "a warning is printed once, not redrawn"
        );
        assert!(s.contains('✔') && s.contains('◐'));
        let warn_at = s.find("anchor withdrawn").unwrap();
        let pair_at = s.rfind("wallet pairing").unwrap();
        assert!(warn_at < pair_at, "warnings print above the board");
    }

    /// What a terminal shows once `s` is written to it, top to bottom:
    /// text, newlines, and the cursor-up, erase-line and erase-below
    /// sequences the board uses. The empty line the cursor ends on is left
    /// out.
    fn screen(s: &str) -> Vec<String> {
        let mut lines = vec![String::new()];
        let mut row = 0;
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            match c {
                '\x1b' => {
                    assert_eq!(chars.next(), Some('['));
                    let mut arg = String::new();
                    let command = loop {
                        let c = chars.next().unwrap();
                        if c.is_ascii_alphabetic() {
                            break c;
                        }
                        arg.push(c);
                    };
                    match command {
                        'A' => row -= arg.parse::<usize>().unwrap_or(1),
                        'K' => lines[row].clear(),
                        'J' => {
                            lines[row].clear();
                            lines.truncate(row + 1);
                        },
                        _ => {},
                    }
                },
                '\n' => {
                    row += 1;
                    if row == lines.len() {
                        lines.push(String::new());
                    }
                },
                c => lines[row].push(c),
            }
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }

    #[test]
    fn a_log_line_lands_above_the_board_and_the_next_frame_is_whole() {
        let (log, _codes, ui) = board(Settings::default(), true);
        ui.step(StepId::Preflight, StepState::Running, "");
        const LINE: &str = "2026-10-02T12:00:00Z  WARN a WalletConnect relay call failed\n";
        if !ui.log_line(LINE.as_bytes()) {
            // What the log writer does without a board: the same terminal.
            log.lock().unwrap().extend_from_slice(LINE.as_bytes());
        }
        ui.step(StepId::Preflight, StepState::Done, "ok");
        ui.status("scan the QR code");
        let shown = screen(&text(&log));
        assert_eq!(shown[0], LINE.trim_end(), "{shown:#?}");
        assert_eq!(
            shown.iter().filter(|l| l.contains("relay call failed")).count(),
            1,
            "{shown:#?}"
        );
        assert_eq!(
            shown.iter().filter(|l| l.contains("preflight")).count(),
            1,
            "no stale row: {shown:#?}"
        );
        assert_eq!(shown.len(), 1 + StepId::ALL.len() + 1, "{shown:#?}");
        assert!(shown[1].starts_with('✔') && shown[1].contains("ok"), "{shown:#?}");
        assert!(shown[10].starts_with("scan the QR code"), "{shown:#?}");
    }

    #[test]
    fn log_lines_go_above_the_board_only_while_it_is_on_the_screen() {
        let (log, _codes, ui) = board(Settings::default(), true);
        ui.take_log_lines();
        assert!(
            !log_above_board(b"before the first frame\n"),
            "nothing to keep clear of yet"
        );
        ui.step(StepId::Anchor, StepState::Running, "");
        assert!(log_above_board(b"WARN while the board is up\n"));
        drop(ui);
        assert!(!log_above_board(b"after the board\n"));
        let s = text(&log);
        assert_eq!(s.matches("while the board is up").count(), 1, "{s}");
        assert!(!s.contains("before the first frame") && !s.contains("after the board"));
    }

    #[test]
    fn the_five_glyphs_of_the_checklist() {
        let all: String = [
            StepState::Done,
            StepState::Running,
            StepState::Pending,
            StepState::Failed,
            StepState::Skipped,
        ]
        .into_iter()
        .map(glyph)
        .collect();
        assert_eq!(all, "✔◐·✖⊘");
    }

    #[test]
    fn the_status_line_counts_attempts_and_errors_print_above_the_board() {
        let (log, _codes, ui) = board(Settings::default(), true);
        ui.step(StepId::Anchor, StepState::Running, "");
        ui.status("waiting for the bridge owner");
        ui.retry("read the anchor", 3, "timed out");
        let s = text(&log);
        let last = visible(&s[s.rfind(&frame_marker()).unwrap()..]);
        let error_at = last
            .find("ERROR read the anchor (attempt 3): timed out")
            .expect("the error line");
        assert!(error_at < last.find("preflight").unwrap(), "{last}");
        let status = last.lines().last().unwrap().to_string();
        assert!(status.contains("waiting for the bridge owner"), "{status}");
        assert!(status.contains("attempt 3, last error 0s ago"), "{status}");
        // A new step starts with a clean status line.
        ui.step(StepId::Anchor, StepState::Done, "");
        let s = text(&log);
        let status = visible(&s[s.rfind(&frame_marker()).unwrap()..])
            .lines()
            .last()
            .unwrap()
            .to_string();
        assert!(
            !status.contains("attempt") && !status.contains("waiting"),
            "{status}"
        );
    }

    #[test]
    fn a_long_row_is_cut_to_the_terminal_width_so_the_redraw_stays_in_place() {
        let (log, out) = capture();
        let (_codes, sink) = capture();
        let ui = TtyBoard::build(
            out,
            Codes {
                out: sink,
                terminal: true,
            },
            Settings::default(),
            || Some(40),
            None,
        );
        ui.step(
            StepId::Deposit,
            StepState::Done,
            &format!("0x{}", "ab".repeat(32)),
        );
        ui.status(&"waiting for confirmations ".repeat(5));
        ui.step(StepId::Anchor, StepState::Running, "line one\nline two");
        let s = text(&log);
        for line in s.split('\n') {
            let v = visible(line);
            assert!(v.chars().count() < 40, "{v:?}");
        }
        // One line per row, whatever the detail held.
        let last = &s[s.rfind(&frame_marker()).unwrap()..];
        assert_eq!(last.matches('\n').count(), StepId::ALL.len() + 1);
    }

    #[test]
    fn a_control_character_in_a_detail_cannot_repaint_the_terminal() {
        let (buf, out) = capture();
        let ui = PlainLines::new(out, true, false);
        ui.warn("node says \x1b[2Jcleared");
        ui.step(StepId::Finalize, StepState::Failed, "\x1b[31mred");
        assert!(!text(&buf).contains('\x1b'), "{:?}", text(&buf));
        let (log, _codes, ui) = board(Settings::default(), true);
        ui.warn("node says \x1b[2Jcleared");
        ui.status("\x1b[31mred");
        // The board's own sequences are cursor-up, erase-line and
        // erase-below; the node's never reach the terminal.
        assert!(!text(&log).contains("\x1b[2J"), "{:?}", text(&log));
        assert!(!text(&log).contains("\x1b[31m"), "{:?}", text(&log));
    }

    #[test]
    fn the_board_prints_the_code_on_stdout_and_draws_itself_again_below_it() {
        let (log, codes, ui) = board(Settings::default(), true);
        ui.step(StepId::Pair, StepState::Running, "scan the QR code");
        ui.qr(PAIRING, &[("network".into(), "Sepolia (11155111)".into())]);
        let c = text(&codes);
        assert!(
            c.contains(&crate::deposit::qr::render_terminal(PAIRING, false).unwrap()),
            "{c}"
        );
        assert!(c.contains("network: Sepolia (11155111)"), "{c}");
        assert!(c.contains(&format!("URI: {PAIRING}")), "{c}");
        // The board left the screen before the code was printed and was
        // drawn fresh below it, so the code stays whole above the board.
        let l = text(&log);
        let erased = l.find("\x1b[J").expect("the board is erased first");
        assert!(l[erased + 3..].starts_with("\x1b[2K"), "{l:?}");
        // The code is on the terminal already: no second copy on the board.
        assert!(!l.contains(PAIRING), "{l:?}");
    }

    #[test]
    fn uri_only_prints_no_picture_and_qr_invert_swaps_the_colours() {
        let (_log, codes, ui) = board(
            Settings {
                uri_only: true,
                ..Settings::default()
            },
            true,
        );
        ui.qr(PAIRING, &[]);
        let c = text(&codes);
        assert!(c.contains(PAIRING), "{c}");
        assert!(!c.contains(['▀', '▄', '█']), "{c}");

        let (_log, codes, ui) = board(
            Settings {
                invert: true,
                ..Settings::default()
            },
            true,
        );
        ui.qr(PAIRING, &[]);
        let c = text(&codes);
        let inverted = crate::deposit::qr::render_terminal(PAIRING, true).unwrap();
        let plain = crate::deposit::qr::render_terminal(PAIRING, false).unwrap();
        assert_ne!(inverted, plain);
        assert!(c.contains(&inverted) && !c.contains(&plain), "{c}");
    }

    #[test]
    fn with_stdout_redirected_the_board_shows_the_code_and_stdout_gets_the_text() {
        let (log, codes, ui) = board(Settings::default(), false);
        ui.qr(PAIRING, &[]);
        let c = text(&codes);
        assert!(c.contains(&format!("URI: {PAIRING}")), "{c}");
        assert!(!c.contains(['▀', '▄', '█']), "no picture into a file: {c}");
        let l = visible(&text(&log));
        assert!(
            l.contains(&crate::deposit::qr::render_terminal(PAIRING, false).unwrap()),
            "{l}"
        );
        assert!(l.contains(PAIRING), "{l}");
    }

    #[test]
    fn plain_lines_print_the_code_as_text_on_stdout_and_log_the_uri() {
        let (log, out) = capture();
        let (codes, sink) = capture();
        let ui = PlainLines::build(
            out,
            Codes {
                out: sink,
                terminal: false,
            },
            Settings::default(),
        );
        ui.qr(PAIRING, &[("function".into(), "deposit".into())]);
        let c = text(&codes);
        assert!(
            c.contains("function: deposit") && c.contains(PAIRING),
            "{c}"
        );
        assert!(!c.contains(['▀', '▄', '█']), "{c}");
        assert!(text(&log).contains(&format!("URI: {PAIRING}")));
        assert!(!text(&log).contains('\x1b'));

        // stdout on a terminal gets the picture, in the colours asked for.
        let (_log, out) = capture();
        let (codes, sink) = capture();
        let ui = PlainLines::build(
            out,
            Codes {
                out: sink,
                terminal: true,
            },
            Settings {
                invert: true,
                ..Settings::default()
            },
        );
        ui.qr(PAIRING, &[]);
        assert!(text(&codes).contains(&crate::deposit::qr::render_terminal(PAIRING, true).unwrap()));
    }

    #[test]
    fn pick_takes_json_first_then_a_terminal_board_then_plain_lines() {
        let run = |g: crate::deposit::args::GlobalFlags, stderr_tty: bool| {
            let (o, stdout) = capture();
            let (e, stderr) = capture();
            let ui = pick_from(&g, false, false, Terminal {
                stdout,
                stdout_tty: false,
                stderr,
                stderr_tty,
                width: || None,
                tick: None,
                logs: false,
            });
            ui.status("hello");
            drop(ui);
            (text(&o), text(&e))
        };
        let json = crate::deposit::args::GlobalFlags {
            json: true,
            ..Default::default()
        };
        let (o, e) = run(json, true);
        let v: serde_json::Value = serde_json::from_str(o.lines().next().unwrap()).unwrap();
        assert_eq!(v["event"], "status");
        assert!(e.is_empty(), "{e}");
        let (o, e) = run(Default::default(), true);
        assert!(
            o.is_empty() && e.contains("\x1b[") && e.contains("hello"),
            "{e:?}"
        );
        let (o, e) = run(Default::default(), false);
        assert!(
            o.is_empty() && !e.contains('\x1b') && e.contains("hello"),
            "{e:?}"
        );
    }

    #[test]
    fn a_running_step_keeps_its_timer_moving_without_events_until_the_board_goes() {
        let (log, out) = capture();
        let (_codes, sink) = capture();
        let ui = TtyBoard::build(
            out,
            Codes {
                out: sink,
                terminal: true,
            },
            Settings::default(),
            || None,
            Some(std::time::Duration::from_millis(5)),
        );
        ui.step(StepId::Prove, StepState::Running, "");
        let frames = || text(&log).matches(&frame_marker()).count();
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while frames() < 3 {
            assert!(
                std::time::Instant::now() < give_up,
                "the board never redrew itself"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Nothing runs: nothing to redraw.
        ui.step(StepId::Prove, StepState::Done, "");
        let settled = frames();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(frames(), settled);
        // Gone: one last frame, then silence.
        ui.step(StepId::Finalize, StepState::Running, "");
        drop(ui);
        let at_drop = text(&log).len();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(text(&log).len(), at_drop);
    }

    #[test]
    fn the_qr_uri_is_printed_whole_by_every_renderer() {
        // The pairing URI is the one secret the wallet must get; the EIP-681
        // URIs carry the transaction. Neither is a URL with an origin.
        let eip681 = "ethereum:0x1c7d4b196cb0c7b01d743fbc6116a902379c7238@11155111/approve?\
                      address=0x0f4f2fc7&uint256=12500000";
        for uri in [PAIRING, eip681] {
            let (log, codes, ui) = board(Settings::default(), false);
            ui.qr(uri, &[]);
            assert!(text(&codes).contains(uri) && text(&log).contains(uri));
            let (buf, out) = capture();
            JsonEvents::new(out, false).qr(uri, &[]);
            let v: serde_json::Value = serde_json::from_str(text(&buf).trim()).unwrap();
            assert_eq!(v["uri"], uri);
        }
    }

    /// A provider URL with a key in its path, in its query and in its
    /// userinfo, as configured: the host in capitals and a port.
    const KEYED: &str = "https://user:PassWord-6f1e@Eth-Sepolia.Example.COM:8443/v2/PathKey-7f3a9c0d?\
                         apikey=QueryKey-51d2e0aa&chain=11155111";

    /// The key material in [`KEYED`].
    const SECRETS: [&str; 3] = ["PassWord-6f1e", "PathKey-7f3a9c0d", "QueryKey-51d2e0aa"];

    /// What an HTTP client's error, a trailing-slash rendering and a `Debug`
    /// print of a parsed URL make of [`KEYED`].
    fn leaks() -> [String; 3] {
        [
            "error sending request for url (https://user:PassWord-6f1e@eth-sepolia.example.com:8443/\
             v2/PathKey-7f3a9c0d?apikey=QueryKey-51d2e0aa&chain=11155111)"
                .into(),
            "error trying to connect: https://eth-sepolia.example.com:8443/v2/PathKey-7f3a9c0d/?\
             apikey=QueryKey-51d2e0aa."
                .into(),
            r#"reqwest::Error { kind: Request, url: Url { scheme: "https", cannot_be_a_base: false, username: "user", password: Some("PassWord-6f1e"), host: Some(Domain("eth-sepolia.example.com")), port: Some(8443), path: "/v2/PathKey-7f3a9c0d", query: Some("apikey=QueryKey-51d2e0aa&chain=11155111"), fragment: None } }"#
                .into(),
        ]
    }

    #[test]
    fn a_url_is_cut_to_its_origin_and_the_configured_parts_are_hidden_anywhere() {
        let r = Redactor::new([KEYED]);
        for l in leaks() {
            let out = r.apply(&l);
            for s in SECRETS {
                assert!(!out.contains(s), "{out}");
            }
        }
        assert_eq!(
            r.apply(&leaks()[0]),
            "error sending request for url (https://eth-sepolia.example.com:8443)"
        );
        assert_eq!(
            r.apply(&leaks()[1]),
            "error trying to connect: https://eth-sepolia.example.com:8443."
        );
        // Without the configured URL only the URL forms are caught.
        assert!(Redactor::default()
            .apply(&leaks()[2])
            .contains("PathKey-7f3a9c0d"));
    }

    #[test]
    fn only_urls_with_an_origin_are_cut_and_short_words_stay() {
        let r = Redactor::default();
        for same in [
            PAIRING,
            "ethereum:0xab@11155111/deposit?uint256=5&int8=0",
            "no url here, just a :// on its own",
            "--network sepolia on 11155111",
        ] {
            assert_eq!(r.apply(same), same);
        }
        assert_eq!(
            r.apply("◐ waiting → https://h.example/x?y=z, then … ✔"),
            "◐ waiting → https://h.example, then … ✔"
        );
        assert_eq!(r.apply("at http://[::1]:8545/key"), "at http://[::1]:8545");
        assert_eq!(
            r.apply("wss://relay.walletconnect.org/?auth=eyJ.a.b&projectId=p1"),
            "wss://relay.walletconnect.org"
        );
        let short = Redactor::new(["https://rpc.example/v2/rpc"]);
        assert_eq!(short.apply("v2 rpc and /v2/"), "v2 rpc and /v2/");
    }

    #[test]
    fn an_at_sign_in_the_path_is_not_taken_for_userinfo() {
        const AT: &str = "https://rpc.example/v2/x@SecretKey99";
        assert_eq!(
            Redactor::default().apply(&format!("url ({AT})")),
            "url (https://rpc.example)"
        );
        let r = Redactor::new([AT]);
        assert_eq!(r.apply(&format!("url ({AT})")), "url (https://rpc.example)");
        for bare in ["SecretKey99 was refused", r#"path: "/v2/x@SecretKey99""#] {
            assert!(!r.apply(bare).contains("SecretKey99"), "{}", r.apply(bare));
        }
        assert!(r.apply("rpc.example").contains("rpc.example"));
        // Userinfo before the host still goes.
        assert_eq!(
            Redactor::default().apply("https://u:PassWord99@rpc.example/v2/k"),
            "https://rpc.example"
        );
    }

    #[test]
    fn a_registered_secret_value_is_hidden_wherever_it_appears() {
        hide_secret_values(["WcProject-0f1e2d3c"]);
        assert_eq!(
            redact("projectId: WcProject-0f1e2d3c, relay"),
            "projectId: <hidden>, relay"
        );
    }

    #[test]
    fn every_renderer_prints_the_rpc_url_without_its_key() {
        hide_url_secrets([KEYED]);
        let feed = |ui: &dyn Ui| {
            let l = leaks();
            ui.step(StepId::EvmConfirm, StepState::Running, &l[1]);
            ui.retry("read the receipt", 2, &l[0]);
            ui.warn(&l[2]);
            ui.status(&l[1]);
            ui.step(StepId::EvmConfirm, StepState::Failed, &l[0]);
            futures::executor::block_on(ui.confirm(&l[1]));
        };
        let (b1, _codes, tty) = board(
            Settings {
                yes: true,
                ..Settings::default()
            },
            true,
        );
        feed(&tty);
        let (b2, out) = capture();
        feed(&PlainLines::new(out, true, false));
        let (b3, out) = capture();
        feed(&JsonEvents::new(out, true));
        for (name, b) in [("board", b1), ("plain", b2), ("json", b3)] {
            let s = text(&b);
            for k in SECRETS {
                assert!(!s.contains(k), "{name}: {s}");
            }
            assert!(
                s.contains("https://eth-sepolia.example.com:8443"),
                "{name}: {s}"
            );
        }
        assert!(!redact_in_deposit_run(&leaks()[2]).contains("PathKey-7f3a9c0d"));
    }
}
