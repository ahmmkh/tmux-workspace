//! Complete AI module boundary, transport, pane decision, retained history, suggest, and action safety tests.

use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use tmux_workspace::ai::core::{
    AiError, AiHost, AiResult, Transport, clean, collect_panes, judge, rank,
};
use tmux_workspace::ai::{self, AiUi, history, panes, suggest};
use tmux_workspace::error::{Error, Result};
use tmux_workspace::snapshot::ProcessTable;
use tmux_workspace::testing::TempDir;
use tmux_workspace::tmux::Fields;
use unicode_normalization::UnicodeNormalization;

const MODEL: &str = "typesafe/jev-1.13";

// ---------------------------------------------------------------------------------------------
// FakeTransport

type PostFn = Box<dyn Fn(&str, &[u8]) -> AiResult<Vec<u8>> + Send + Sync>;
type KeyFn = Box<dyn Fn() -> AiResult<String> + Send + Sync>;

struct FakeTransport {
    model: String,
    key: KeyFn,
    post: PostFn,
}

impl FakeTransport {
    fn new(post: impl Fn(&str, &[u8]) -> AiResult<Vec<u8>> + Send + Sync + 'static) -> Self {
        Self {
            model: MODEL.into(),
            key: Box::new(|| Ok("synthetic-test-key".into())),
            post: Box::new(post),
        }
    }

    fn with_key(mut self, key: impl Fn() -> AiResult<String> + Send + Sync + 'static) -> Self {
        self.key = Box::new(key);
        self
    }

    fn answers(answers: Value) -> Self {
        Self::new(move |_, _| {
            Ok(serde_json::to_vec(&json!({"model": MODEL, "answers": answers})).unwrap())
        })
    }

    fn raw_body(body: Vec<u8>) -> Self {
        Self::new(move |_, _| Ok(body.clone()))
    }

    fn err(err: AiError) -> Self {
        Self::new(move |_, _| Err(err.clone()))
    }
}

impl Transport for FakeTransport {
    fn model(&self) -> AiResult<String> {
        Ok(self.model.clone())
    }

    fn api_key(&self) -> AiResult<String> {
        (self.key)()
    }

    fn post_systemone(&self, api_key: &str, body: &[u8]) -> AiResult<Vec<u8>> {
        (self.post)(api_key, body)
    }
}

fn score_all_2(_key: &str, body: &[u8]) -> AiResult<Vec<u8>> {
    let payload: Value = serde_json::from_slice(body).unwrap();
    let mut answers = Map::new();
    for k in payload["questions"].as_object().unwrap().keys() {
        answers.insert(
            k.clone(),
            json!({
                "type": "score",
                "score": 2.0,
                "confidence": 1.0,
                "probabilities": {"0": 0.0, "1": 0.0, "2": 1.0},
            }),
        );
    }
    Ok(serde_json::to_vec(&json!({"model": MODEL, "answers": answers})).unwrap())
}

fn choice_answer(question: &Value, selected: &str) -> Value {
    let mut probs = Map::new();
    for label in question["criteria"].as_object().unwrap().keys() {
        probs.insert(
            label.clone(),
            json!(if label == selected { 1.0 } else { 0.0 }),
        );
    }
    json!({
        "type": "choice",
        "choice": selected,
        "confidence": 1.0,
        "probabilities": probs,
    })
}

fn attention_response(
    questions: &Map<String, Value>,
    decisions: &HashMap<(String, String), (f64, String)>,
) -> Value {
    let mut answers = Map::new();
    for (key, question) in questions {
        let parts: Vec<&str> = key.split('_').collect();
        let name = parts[0].to_string();
        let cond = parts[1].to_string();
        let (prob, sel) = decisions
            .get(&(name, cond))
            .cloned()
            .unwrap_or((0.0, "none".into()));
        if parts.len() > 2 {
            answers.insert(key.clone(), choice_answer(question, &sel));
        } else {
            answers.insert(key.clone(), json!({"type": "noul", "noul": prob}));
        }
    }
    json!({"model": MODEL, "answers": answers})
}

// ---------------------------------------------------------------------------------------------
// FakeTmuxHost (stateful fake tmux boundary; production collection and actions stay real)

#[derive(Clone, Debug, PartialEq, Eq)]
struct PaneRow {
    pid: String,
    session: String,
    window_id: String,
    window_index: String,
    pane_id: String,
    pane_index: String,
    pane_pid: String,
    cwd: String,
    command: String,
    workspace_id: String,
    label: String,
    app: String,
    excerpt: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Focus {
    session: String,
    window: String,
    pane: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TmuxState {
    panes: Vec<PaneRow>,
    windows: Vec<(String, String)>,
    options: HashMap<String, HashMap<String, String>>,
    global_options: HashMap<String, String>,
    focus: Focus,
    current_pane: Option<String>,
    forbid_capture: bool,
}

type ListHook = Box<dyn Fn(&mut TmuxState) + Send + Sync>;

struct FakeTmuxHost {
    state: Mutex<TmuxState>,
    state_root: PathBuf,
    share_root: PathBuf,
    skill_roots: Vec<PathBuf>,
    on_list: Option<ListHook>,
}

impl FakeTmuxHost {
    fn empty(root: PathBuf) -> Self {
        Self {
            state: Mutex::new(TmuxState {
                panes: Vec::new(),
                windows: Vec::new(),
                options: HashMap::new(),
                global_options: HashMap::new(),
                focus: Focus {
                    session: "work".into(),
                    window: "@1".into(),
                    pane: "%1".into(),
                },
                current_pane: Some("%1".into()),
                forbid_capture: false,
            }),
            state_root: root.clone(),
            share_root: root,
            skill_roots: Vec::new(),
            on_list: None,
        }
    }

    fn action_fixture() -> Self {
        let host = Self::empty(PathBuf::from("/tmp/fake-ai-actions"));
        host.add_action_pane("%1", "@1", "work", "editor");
        host.add_action_pane("%2", "@2", "__parked", "selected");
        host.add_action_pane("%3", "@3", "__parked", "recent");
        host.add_action_pane("%4", "@4", "other", "review");
        host
    }

    fn add_action_pane(&self, pane_id: &str, window_id: &str, session: &str, name: &str) {
        let mut st = self.state.lock().unwrap();
        if let Some(existing) = st.windows.iter_mut().find(|(id, _)| id == window_id) {
            existing.1 = name.to_string();
        } else {
            st.windows.push((window_id.to_string(), name.to_string()));
        }
        let num: usize = pane_id[1..].parse().unwrap_or(1);
        st.panes.push(PaneRow {
            pid: "700".into(),
            session: session.into(),
            window_id: window_id.into(),
            window_index: window_id[1..].into(),
            pane_id: pane_id.into(),
            pane_index: "0".into(),
            pane_pid: format!("{}", 1000 + num),
            cwd: format!("/synthetic/{name}"),
            command: "zsh".into(),
            workspace_id: format!("stable-{pane_id}"),
            label: name.into(),
            app: String::new(),
            excerpt: format!("Synthetic output for {pane_id}"),
        });
        let opts = st.options.entry(pane_id.into()).or_default();
        if session == "__parked" {
            opts.insert("@workspace_parked".into(), "1".into());
            opts.insert("@workspace_home".into(), "work:@1".into());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_simple_pane(
        &self,
        num: usize,
        window_id: &str,
        window_name: &str,
        label: &str,
        cwd: &str,
        parked: bool,
        shared: bool,
        excerpt: &str,
    ) {
        let mut st = self.state.lock().unwrap();
        let pane_id = format!("%{num}");
        if !st.windows.iter().any(|(id, _)| id == window_id) {
            st.windows
                .push((window_id.to_string(), window_name.to_string()));
        }
        st.panes.push(PaneRow {
            pid: "700".into(),
            session: if parked { "__parked" } else { "synthetic" }.into(),
            window_id: window_id.into(),
            window_index: "0".into(),
            pane_id: pane_id.clone(),
            pane_index: num.to_string(),
            pane_pid: format!("{}", 1000 + num),
            cwd: cwd.into(),
            command: "zsh".into(),
            workspace_id: format!("{:032x}", num + 1),
            label: label.into(),
            app: String::new(),
            excerpt: excerpt.into(),
        });
        let opts = st.options.entry(pane_id).or_default();
        if shared {
            opts.insert("@workspace_ai_share".into(), "1".into());
        }
    }

    fn snapshot(&self) -> TmuxState {
        self.state.lock().unwrap().clone()
    }

    fn pane_value(&self, pane_id: &str) -> Value {
        collect_panes(self, false, None)
            .unwrap()
            .into_iter()
            .find(|p| p["pane_id"] == pane_id)
            .unwrap()
    }

    fn proposal(&self, window_id: &str) -> Value {
        let prev = self
            .state
            .lock()
            .unwrap()
            .windows
            .iter()
            .find(|(id, _)| id == window_id)
            .unwrap()
            .1
            .clone();
        let mut panes_map = Map::new();
        for p in collect_panes(self, false, None).unwrap() {
            if p["window_id"] == window_id {
                panes_map.insert(
                    p["pane_id"].as_str().unwrap().to_string(),
                    p["fingerprint"].clone(),
                );
            }
        }
        json!({
            "window_id": window_id,
            "previous_name": prev,
            "name": "Billing review",
            "panes": panes_map,
        })
    }

    fn change(&self, pane_id: &str, kind: &str) {
        let mut st = self.state.lock().unwrap();
        if kind == "deleted" {
            st.panes.retain(|p| p.pane_id != pane_id);
            st.options.remove(pane_id);
            return;
        }
        let pane = st.panes.iter_mut().find(|p| p.pane_id == pane_id).unwrap();
        match kind {
            "reused" => {
                pane.pane_pid = "9999".into();
                pane.workspace_id = "replacement".into();
            }
            "cwd" => pane.cwd = "/synthetic/new-task".into(),
            "process" => pane.command = "python".into(),
            "server-restarted" => pane.pid = "701".into(),
            "moved" => {
                pane.window_id = "@4".into();
                pane.window_index = "4".into();
                pane.session = "other".into();
            }
            "parked" => {
                pane.window_id = "@3".into();
                pane.window_index = "3".into();
                pane.session = "__parked".into();
            }
            other => panic!("unknown change {other}"),
        }
    }
}

impl AiHost for FakeTmuxHost {
    fn list_panes(&self, fields: &[&str]) -> Result<Vec<Fields>> {
        let mut st = self.state.lock().unwrap();
        if let Some(hook) = &self.on_list {
            hook(&mut st);
        }
        let mut out = Vec::new();
        for p in &st.panes {
            let win_name = st
                .windows
                .iter()
                .find(|(id, _)| *id == p.window_id)
                .map(|(_, n)| n.as_str())
                .unwrap_or_default();
            let opts = st.options.get(&p.pane_id);
            let opt = |k: &str| -> String {
                opts.and_then(|m| m.get(k))
                    .or_else(|| st.global_options.get(k))
                    .cloned()
                    .unwrap_or_default()
            };
            let mut row = Fields::default();
            for &name in fields {
                let val = match name {
                    "pid" => p.pid.clone(),
                    "session_name" => p.session.clone(),
                    "window_id" => p.window_id.clone(),
                    "window_name" => win_name.to_string(),
                    "window_index" => p.window_index.clone(),
                    "pane_id" => p.pane_id.clone(),
                    "pane_index" => p.pane_index.clone(),
                    "pane_pid" => p.pane_pid.clone(),
                    "pane_current_path" => p.cwd.clone(),
                    "pane_current_command" => p.command.clone(),
                    "@workspace_id" => p.workspace_id.clone(),
                    "@workspace_label" => p.label.clone(),
                    "@workspace_app" => p.app.clone(),
                    other => opt(other),
                };
                row.set(name, val);
            }
            out.push(row);
        }
        Ok(out)
    }

    fn list_windows(&self, _fields: &[&str]) -> Result<Vec<Fields>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .windows
            .iter()
            .map(|(id, name)| {
                let mut f = Fields::default();
                f.set("window_id", id);
                f.set("window_name", name);
                f
            })
            .collect())
    }

    fn pane_option(&self, name: &str, pane_id: &str) -> String {
        let st = self.state.lock().unwrap();
        st.options
            .get(pane_id)
            .and_then(|m| m.get(name))
            .cloned()
            .unwrap_or_default()
    }

    fn capture_pane(&self, pane_id: &str) -> Result<String> {
        let st = self.state.lock().unwrap();
        if st.forbid_capture {
            panic!("Unshared pane output was read!");
        }
        Ok(st
            .panes
            .iter()
            .find(|p| p.pane_id == pane_id)
            .map(|p| p.excerpt.clone())
            .unwrap_or_default())
    }

    fn process_table(&self) -> Result<ProcessTable> {
        Ok(HashMap::new())
    }

    fn live_app(&self, _row: &Fields, _table: &ProcessTable) -> Option<Value> {
        None
    }

    fn run_tmux(&self, args: &[&str]) -> Result<String> {
        let mut st = self.state.lock().unwrap();
        let cmd = args[0];
        let target = args
            .iter()
            .position(|a| *a == "-t")
            .and_then(|i| args.get(i + 1))
            .copied();
        match cmd {
            "switch-client" => {
                st.focus.session = target.unwrap().trim_start_matches('=').to_string();
            }
            "select-window" => st.focus.window = target.unwrap().to_string(),
            "select-pane" => st.focus.pane = target.unwrap().to_string(),
            "join-pane" => {
                let src = args
                    .iter()
                    .position(|a| *a == "-s")
                    .and_then(|i| args.get(i + 1))
                    .copied()
                    .unwrap();
                let dst = target.unwrap();
                let dest = st
                    .panes
                    .iter()
                    .find(|p| p.pane_id == dst)
                    .cloned()
                    .ok_or_else(|| Error::Msg("tmux target disappeared".into()))?;
                let source = st
                    .panes
                    .iter_mut()
                    .find(|p| p.pane_id == src)
                    .ok_or_else(|| Error::Msg("tmux target disappeared".into()))?;
                source.session = dest.session.clone();
                source.window_id = dest.window_id.clone();
                source.window_index = dest.window_index.clone();
                st.focus = Focus {
                    session: dest.session,
                    window: dest.window_id,
                    pane: src.to_string(),
                };
            }
            "set-option" => {
                let t = target.unwrap().to_string();
                let t_idx = args.iter().position(|a| *a == "-t").unwrap();
                let opt_name = args[t_idx + 2];
                if !st.panes.iter().any(|p| p.pane_id == t) {
                    return Err(Error::Msg("tmux target disappeared".into()));
                }
                if args.contains(&"-pu") {
                    if let Some(m) = st.options.get_mut(&t) {
                        m.remove(opt_name);
                    }
                } else {
                    let val = *args.last().unwrap();
                    st.options
                        .entry(t)
                        .or_default()
                        .insert(opt_name.to_string(), val.to_string());
                }
            }
            "rename-window" => {
                let t = target.unwrap();
                let new_name = *args.last().unwrap();
                let win = st
                    .windows
                    .iter_mut()
                    .find(|(id, _)| id == t)
                    .ok_or_else(|| Error::Msg("tmux target disappeared".into()))?;
                win.1 = new_name.to_string();
            }
            other => panic!("Unexpected tmux mutation: {other}"),
        }
        Ok(String::new())
    }

    fn state_root(&self) -> PathBuf {
        self.state_root.clone()
    }

    fn current_pane(&self) -> Option<String> {
        self.state.lock().unwrap().current_pane.clone()
    }

    fn share_root(&self) -> PathBuf {
        self.share_root.clone()
    }

    fn self_exe(&self) -> String {
        "/usr/local/bin/tmux-workspace".into()
    }

    fn skill_roots(&self) -> Vec<PathBuf> {
        self.skill_roots.clone()
    }
}

// ---------------------------------------------------------------------------------------------
// FakeUi

type PromptFn = Box<dyn Fn(&str) -> Result<String>>;

struct FakeUi {
    tty: bool,
    prompt_fn: PromptFn,
    stdout: RefCell<Vec<String>>,
    stderr: RefCell<Vec<String>>,
}

impl FakeUi {
    fn answers(answers: &[&str]) -> Self {
        let queue = RefCell::new(
            answers
                .iter()
                .map(|s| (*s).to_string())
                .collect::<std::collections::VecDeque<_>>(),
        );
        Self {
            tty: true,
            prompt_fn: Box::new(move |_| Ok(queue.borrow_mut().pop_front().unwrap_or_default())),
            stdout: RefCell::new(Vec::new()),
            stderr: RefCell::new(Vec::new()),
        }
    }

    fn forbid_prompt() -> Self {
        Self {
            tty: true,
            prompt_fn: Box::new(|p| panic!("Unexpected prompt: {p}")),
            stdout: RefCell::new(Vec::new()),
            stderr: RefCell::new(Vec::new()),
        }
    }

    fn custom(tty: bool, f: impl Fn(&str) -> Result<String> + 'static) -> Self {
        Self {
            tty,
            prompt_fn: Box::new(f),
            stdout: RefCell::new(Vec::new()),
            stderr: RefCell::new(Vec::new()),
        }
    }
}

impl AiUi for FakeUi {
    fn stdin_is_tty(&self) -> bool {
        self.tty
    }

    fn stdout_is_tty(&self) -> bool {
        self.tty
    }

    fn has_fzf(&self) -> bool {
        false
    }

    fn prompt(&self, prompt: &str) -> Result<String> {
        (self.prompt_fn)(prompt)
    }

    fn println(&self, line: &str) {
        self.stdout.borrow_mut().push(line.to_string());
    }

    fn eprintln(&self, line: &str) {
        self.stderr.borrow_mut().push(line.to_string());
    }
}

// =============================================================================================
// 1. Model output boundaries & transport boundaries (test_ai_boundaries.py + test_transport.py)

#[test]
fn closed_judgments_reject_missing_extra_and_invented_answers() {
    let mut questions = Map::new();
    questions.insert(
        "match".into(),
        json!({"type": "choice", "instructions": "Does this match?", "criteria": {"yes": null, "no": null}}),
    );
    let invalid = [
        json!({}),
        json!({"match": "yes"}),
        json!({"match": true}),
        json!({"match": {"type": "choice", "choice": "execute", "probabilities": {"yes": 1, "no": 0}, "confidence": 1}}),
        json!({"match": {"type": "noul", "noul": 1}}),
        json!({"match": {}, "command": "kill-pane"}),
    ];
    for ans in invalid {
        let t = FakeTransport::answers(ans);
        assert!(judge(&t, &json!({}), &questions, None).is_err());
    }
}

#[test]
fn invalid_probabilities_inconsistent_scores_and_huge_numbers_are_rejected() {
    let mut noul_q = Map::new();
    noul_q.insert(
        "q".into(),
        json!({"type": "noul", "instructions": "Does this match?"}),
    );
    for raw_body in [
        r#"{"model":"typesafe/jev-1.13","answers":{"q":{"type":"noul","noul":-0.1}}}"#,
        r#"{"model":"typesafe/jev-1.13","answers":{"q":{"type":"noul","noul":1.1}}}"#,
        r#"{"model":"typesafe/jev-1.13","answers":{"q":{"type":"noul","noul":true}}}"#,
        &format!(
            r#"{{"model":"typesafe/jev-1.13","answers":{{"q":{{"type":"noul","noul":1{}}}}}}}"#,
            "0".repeat(400)
        ),
    ] {
        let t = FakeTransport::raw_body(raw_body.as_bytes().to_vec());
        assert!(judge(&t, &json!({}), &noul_q, None).is_err(), "{raw_body}");
    }

    let mut score_q = Map::new();
    score_q.insert(
        "q".into(),
        json!({"type": "score", "instructions": "How relevant?", "criteria": ["Unrelated", "Direct"]}),
    );
    for probs in [
        json!({"0": 0, "1": 0}),
        json!({"0": 1, "1": 0}),
        json!({"0": 0, "other": 1}),
    ] {
        let t = FakeTransport::answers(
            json!({"q": {"type": "score", "score": 1, "probabilities": probs, "confidence": 1}}),
        );
        assert!(judge(&t, &json!({}), &score_q, None).is_err());
    }
}

#[test]
fn bad_json_wrong_backend_and_incomplete_responses_fail_closed() {
    let mut q = Map::new();
    q.insert(
        "q".into(),
        json!({"type": "noul", "instructions": "Ready?"}),
    );
    let bodies: &[&[u8]] = &[
        b"not json",
        b"null",
        b"[]",
        br#"{"model":"typesafe/jev-1.13"}"#,
        br#"{"model":"openai/gpt-4.1-mini","answers":{"q":{"type":"noul","noul":1}}}"#,
        br#"{"model":"typesafe/jev-1.13","answers":{"q":{"type":"noul","noul":1},"other":{}}}"#,
    ];
    for body in bodies {
        let t = FakeTransport::raw_body(body.to_vec());
        assert!(judge(&t, &json!({"text": "Approve?"}), &q, None).is_err());
    }
}

#[test]
fn oversize_evidence_rejected_before_network_and_blank_query_stays_local() {
    let mut q = Map::new();
    q.insert(
        "q".into(),
        json!({"type": "noul", "instructions": "Ready?"}),
    );
    let forbid = FakeTransport::new(|_, _| panic!("Network must not be called"));
    assert!(judge(&forbid, &json!({"text": "x".repeat(180_001)}), &q, None).is_err());

    let records = vec![json!({"id": "private", "title": "private", "detail": "PRIVATE_CATALOG"})];
    let (items, _) = rank(&forbid, "   ", &records, None);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "private");
    assert!(items[0].get("judgment").is_none());
}

#[test]
fn ranking_orders_expected_relevance_redacts_secrets_handles_ties_and_batches_multibyte() {
    let records: Vec<Value> = ["weak", "strong", "unrelated"]
        .into_iter()
        .map(|n| json!({"id": n, "title": n, "detail": n}))
        .collect();
    let t = FakeTransport::answers(json!({
        "candidate_0": {"type": "score", "score": 1.5, "probabilities": {"0": 0.1, "1": 0.3, "2": 0.6}, "confidence": 0.5},
        "candidate_1": {"type": "score", "score": 1.9, "probabilities": {"0": 0.0, "1": 0.1, "2": 0.9}, "confidence": 0.5},
        "candidate_2": {"type": "score", "score": 0.0, "probabilities": {"0": 1.0, "1": 0.0, "2": 0.0}, "confidence": 0.5},
    }));
    let (items, _) = rank(&t, "task", &records, None);
    let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["strong", "weak"]);

    // Tied no-match (0.5 vs 0.5) drops item.
    let tied = FakeTransport::answers(json!({
        "candidate_0": {"type": "score", "score": 1.0, "probabilities": {"0": 0.5, "1": 0.0, "2": 0.5}, "confidence": 0.0},
    }));
    let (items, _) = rank(
        &tied,
        "task",
        &[json!({"id": "one", "title": "one", "detail": "src"})],
        None,
    );
    assert!(items.is_empty());

    // Secrets redacted before ranking request.
    let secret = format!("sk-or-v1-{}", "z".repeat(40));
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_c = Arc::clone(&seen);
    let t = FakeTransport::new(move |k, body| {
        seen_c
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(body).into_owned());
        score_all_2(k, body)
    });
    let (items, _) = rank(
        &t,
        "billing",
        &[
            json!({"id": "one", "title": format!("billing {secret}"), "detail": format!("OPENROUTER_API_KEY={secret}")}),
        ],
        None,
    );
    assert_eq!(items.len(), 1);
    assert!(!seen.lock().unwrap().join("").contains(&secret));

    // Multibyte catalog splits cleanly across bounded requests <= 180_000 bytes.
    let mb_records: Vec<Value> = (0..75)
        .map(|i| json!({"id": i.to_string(), "title": format!("invoice {i}"), "detail": "فاتورة".repeat(650)}))
        .collect();
    let transmitted = Arc::new(Mutex::new(Vec::<String>::new()));
    let tr_c = Arc::clone(&transmitted);
    let t = FakeTransport::new(move |k, body| {
        assert!(body.len() <= 180_000);
        let payload: Value = serde_json::from_slice(body).unwrap();
        for c in payload["state"]["candidates"].as_array().unwrap() {
            tr_c.lock()
                .unwrap()
                .push(c["title"].as_str().unwrap().to_string());
        }
        score_all_2(k, body)
    });
    let (items, _) = rank(&t, "invoice", &mb_records, None);
    assert_eq!(items.len(), 75);
    assert_eq!(transmitted.lock().unwrap().len(), 75);

    // One failed batch falls back cleanly to complete local catalog without judgments.
    let big: Vec<Value> = (0..50)
        .map(|i| json!({"id": i.to_string(), "title": format!("task {i}"), "detail": "data ".repeat(800)}))
        .collect();
    let t = FakeTransport::new(|k, body| {
        let payload: Value = serde_json::from_slice(body).unwrap();
        if payload["state"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["title"] == "task 49")
        {
            return Err(AiError::Other("synthetic timeout".into()));
        }
        score_all_2(k, body)
    });
    let (items, _) = rank(&t, "task", &big, None);
    assert_eq!(items.len(), 50);
    assert!(
        items
            .iter()
            .all(|i| i["relevance"] == "local" && i.get("judgment").is_none())
    );
}

#[test]
fn http_failures_keep_local_catalog_and_common_secrets_are_cleaned() {
    let records = vec![
        json!({"id": "one", "title": "billing", "detail": "billing task"}),
        json!({"id": "two", "title": "other", "detail": "other task"}),
    ];
    for msg in [
        "OpenRouter HTTP 401; check credit, credentials, Jev and ZDR availability.",
        "OpenRouter HTTP 429; check credit, credentials, Jev and ZDR availability.",
        "OpenRouter unavailable or returned invalid Jev output.",
    ] {
        let t = FakeTransport::err(AiError::Other(msg.into()));
        let (items, notice) = rank(&t, "billing", &records, None);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i["relevance"] == "local"));
        assert!(!notice.contains("synthetic-test-key"));
    }

    let secret = format!("sk-or-v1-{}", "a".repeat(32));
    let output = clean(&format!(
        "\x1b[31mhello\x1b[0m\nOPENROUTER_API_KEY={secret}\nBearer abcdef1234\npostgres://alice:privatepass@db/app"
    ));
    assert!(!output.contains(&secret));
    assert!(!output.contains("abcdef1234"));
    assert!(!output.contains("privatepass"));
    assert!(!output.contains('\x1b'));
    assert!(output.contains("hello"));
}

#[test]
fn inherited_tmux_option_cannot_grant_terminal_access() {
    let host = FakeTmuxHost::empty(PathBuf::from("/tmp/none"));
    host.add_simple_pane(0, "@0", "shell", "shell", "/tmp", false, false, "SECRET");
    {
        let mut st = host.state.lock().unwrap();
        st.global_options
            .insert("@workspace_ai_share".into(), "1".into());
        st.forbid_capture = true;
    }
    let collected = collect_panes(&host, true, None).unwrap();
    assert_eq!(collected[0]["shared"], json!(false));
    assert_eq!(collected[0]["excerpt"], json!(""));
}

// =============================================================================================
// 2. Pane decisions: find, attention, name_tab (test_pane_decisions.py)

#[test]
fn revocation_during_credentials_aborts_find_attention_and_name_tab() {
    for feature in ["find", "attention", "name_tab"] {
        let host = Arc::new(FakeTmuxHost::empty(PathBuf::from("/tmp/none")));
        host.add_simple_pane(
            0,
            "@0",
            "shell",
            "shell",
            "/tmp",
            false,
            true,
            "Please approve invoice reconciliation.",
        );
        host.state.lock().unwrap().current_pane = Some("%0".into());
        let host_c = Arc::clone(&host);
        let transport = FakeTransport::new(|_, _| panic!("Revoked content reached model"))
            .with_key(move || {
                host_c
                    .state
                    .lock()
                    .unwrap()
                    .options
                    .get_mut("%0")
                    .unwrap()
                    .remove("@workspace_ai_share");
                Ok("synthetic-key".into())
            });
        let (items, _) = match feature {
            "find" => panes::find(host.as_ref(), &transport, "invoice reconciliation").unwrap(),
            "attention" => panes::attention(host.as_ref(), &transport).unwrap(),
            _ => panes::name_tab(host.as_ref(), &transport, "Invoice reconciliation").unwrap(),
        };
        assert!(items.is_empty(), "feature={feature}");
    }
}

#[test]
fn attention_routes_independent_conditions_thresholds_batches_oversized_and_unshared() {
    // 1. All 3 independent conditions routed with exact source lines.
    let host = FakeTmuxHost::empty(PathBuf::from("/tmp/none"));
    let src = "Please approve deployment.\nBlocked by missing schema.\nReview completed changes.";
    host.add_simple_pane(0, "@0", "shell", "shell", "/tmp", false, true, src);
    let decisions = HashMap::from([
        (("P000".into(), "input".into()), (0.9, "L001".into())),
        (("P000".into(), "blocker".into()), (0.9, "L002".into())),
        (("P000".into(), "review".into()), (0.9, "L003".into())),
    ]);
    let t = FakeTransport::new(move |_, body| {
        let payload: Value = serde_json::from_slice(body).unwrap();
        let resp = attention_response(payload["questions"].as_object().unwrap(), &decisions);
        Ok(serde_json::to_vec(&resp).unwrap())
    });
    let (items, _) = panes::attention(&host, &t).unwrap();
    let ids: HashSet<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        HashSet::from([
            "attention:%0:input",
            "attention:%0:blocker",
            "attention:%0:review"
        ])
    );

    // 2. Threshold edge cases: 0.5 -> unclassified, 0.500001 -> input, 0.499999 + none -> omitted.
    let host = FakeTmuxHost::empty(PathBuf::from("/tmp/none"));
    for n in 0..3 {
        host.add_simple_pane(n, "@0", "shell", "shell", "/tmp", false, true, "Approve?");
    }
    let decisions = HashMap::from([
        (("P000".into(), "input".into()), (0.5, "L001".into())),
        (("P001".into(), "input".into()), (0.500001, "L001".into())),
        (("P002".into(), "input".into()), (0.499999, "none".into())),
    ]);
    let t = FakeTransport::new(move |_, body| {
        let payload: Value = serde_json::from_slice(body).unwrap();
        Ok(serde_json::to_vec(&attention_response(
            payload["questions"].as_object().unwrap(),
            &decisions,
        ))
        .unwrap())
    });
    let (items, _) = panes::attention(&host, &t).unwrap();
    let ids: HashSet<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        HashSet::from(["attention:%0:unclassified", "attention:%1:input"])
    );

    // 3. Unshared excluded; no consent never calls model.
    let host = FakeTmuxHost::empty(PathBuf::from("/tmp/none"));
    host.add_simple_pane(0, "@0", "shell", "shell", "/tmp", false, false, "PRIVATE");
    let forbid = FakeTransport::new(|_, _| panic!("Must not call model without consent"));
    let (items, notice) = panes::attention(&host, &forbid).unwrap();
    assert!(items.is_empty() && !notice.is_empty());

    // 4. Failed batch in 17 panes degrades only that batch; oversized sources stay local.
    let host = FakeTmuxHost::empty(PathBuf::from("/tmp/none"));
    for n in 0..17 {
        host.add_simple_pane(
            n,
            "@0",
            "shell",
            "shell",
            "/tmp",
            false,
            true,
            &format!("Approval request {n}"),
        );
    }
    let t = FakeTransport::new(|_, body| {
        let payload: Value = serde_json::from_slice(body).unwrap();
        let panes_obj = payload["state"]["panes"].as_object().unwrap();
        assert!(panes_obj.len() <= 8);
        if panes_obj.contains_key("P008") {
            return Err(AiError::Other("Synthetic outage".into()));
        }
        let decisions: HashMap<(String, String), (f64, String)> = panes_obj
            .keys()
            .map(|k| ((k.clone(), "input".into()), (0.9, "L001".into())))
            .collect();
        Ok(serde_json::to_vec(&attention_response(
            payload["questions"].as_object().unwrap(),
            &decisions,
        ))
        .unwrap())
    });
    let (items, notice) = panes::attention(&host, &t).unwrap();
    assert_eq!(items.len(), 17);
    assert!(notice.contains("Synthetic outage"));
    assert!(items.iter().any(|i| i["id"] == "attention:%8:unclassified"));
    assert!(items.iter().any(|i| i["id"] == "attention:%0:input"));

    // Oversized (>253 lines or >60KB JSON) stays local while small source is judged.
    let host = FakeTmuxHost::empty(PathBuf::from("/tmp/none"));
    let long_lines = (0..254)
        .map(|n| format!("OV{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let long_bytes = (0..252)
        .map(|_| format!("B{}", "界".repeat(13)))
        .collect::<Vec<_>>()
        .join("\n");
    host.add_simple_pane(0, "@0", "shell", "shell", "/tmp", false, true, &long_lines);
    host.add_simple_pane(1, "@0", "shell", "shell", "/tmp", false, true, &long_bytes);
    host.add_simple_pane(2, "@0", "shell", "shell", "/tmp", false, true, "Approve?");
    let seen = Arc::new(Mutex::new(String::new()));
    let seen_c = Arc::clone(&seen);
    let t = FakeTransport::new(move |_, body| {
        seen_c
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(body));
        let payload: Value = serde_json::from_slice(body).unwrap();
        let decisions: HashMap<(String, String), (f64, String)> = payload["state"]["panes"]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| ((k.clone(), "input".into()), (0.9, "L001".into())))
            .collect();
        Ok(serde_json::to_vec(&attention_response(
            payload["questions"].as_object().unwrap(),
            &decisions,
        ))
        .unwrap())
    });
    let (items, _) = panes::attention(&host, &t).unwrap();
    let ids: HashSet<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        HashSet::from([
            "attention:%0:unclassified",
            "attention:%1:unclassified",
            "attention:%2:input"
        ])
    );
    let req = seen.lock().unwrap();
    assert!(!req.contains("OV0"));
    assert!(!req.contains("界界界"));
}

#[test]
fn name_tab_handles_unicode_marks_safety_filters_and_window_scoping() {
    // Arabic, decomposed Latin, Devanagari spacing marks, and ASCII all survive NFC & \p{M}.
    for hint in [
        "مراجعة الفواتير",
        "Cafe\u{0301} billing",
        "हिन्दी भुगतान",
        "client-onboarding",
    ] {
        let host = FakeTmuxHost::empty(PathBuf::from("/tmp/none"));
        host.add_simple_pane(0, "@0", "shell", "shell", "/tmp", false, false, "");
        host.state.lock().unwrap().current_pane = Some("%0".into());
        let expected: String = hint.nfc().collect();
        let exp_c = expected.clone();
        let t = FakeTransport::new(move |_, body| {
            let payload: Value = serde_json::from_slice(body).unwrap();
            let cands = payload["state"]["candidates"].as_object().unwrap();
            let key = cands
                .iter()
                .find(|(_, v)| v["text"] == exp_c)
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| panic!("Expected candidate {exp_c:?} in {cands:?}"));
            let ans = choice_answer(&payload["questions"]["title"], &key);
            Ok(serde_json::to_vec(&json!({"model": MODEL, "answers": {"title": ans}})).unwrap())
        });
        let (items, _) = panes::name_tab(&host, &t, hint).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["rename"]["name"], json!(expected));
    }

    // Unsafe hints never reach the model; generic-only makes no request.
    let forbid = FakeTransport::new(|_, _| panic!("Must not call model"));
    for unsafe_hint in [
        "$(touch /tmp/owned)",
        "#{pane_id}",
        "secret=do-not-copy",
        &"x".repeat(41),
        "shell agent terminal",
    ] {
        let host = FakeTmuxHost::empty(PathBuf::from("/tmp/none"));
        host.add_simple_pane(
            0,
            "@0",
            "shell",
            "shell",
            "/tmp",
            false,
            false,
            "UNSHARED_PRIVATE_TEXT",
        );
        host.state.lock().unwrap().current_pane = Some("%0".into());
        let (items, _) = panes::name_tab(&host, &forbid, unsafe_hint).unwrap();
        assert!(items.is_empty(), "hint={unsafe_hint:?}");
    }

    // Unsafe excerpts (fenced, indented, commands, secrets) and other-window sources excluded.
    let host = FakeTmuxHost::empty(PathBuf::from("/tmp/none"));
    let unsafe_excerpt = [
        "$(touch /tmp/SHELL_MARKER)",
        "#{TMUX_MARKER}",
        "password=PASSWORD_MARKER",
        "Bearer BEARER_MARKER",
        "curl https://EXFILTRATION_MARKER.invalid",
        "git checkout COMMAND_MARKER",
        "```\nFENCED_MARKER\n```",
        "    INDENTED_MARKER",
        "Payment reconciliation",
    ]
    .join("\n");
    host.add_simple_pane(
        0,
        "@0",
        "shell",
        "shell",
        "/tmp",
        false,
        false,
        "PRIVATE_NAMING_MARKER",
    );
    host.add_simple_pane(
        1,
        "@0",
        "shell",
        "Billing review",
        "/tmp",
        false,
        true,
        &unsafe_excerpt,
    );
    host.add_simple_pane(
        2,
        "@2",
        "other",
        "OTHER_WINDOW_MARKER",
        "/tmp",
        false,
        true,
        "OTHER_EXCERPT_MARKER",
    );
    host.state.lock().unwrap().current_pane = Some("%0".into());
    let seen = Arc::new(Mutex::new(String::new()));
    let seen_c = Arc::clone(&seen);
    let t = FakeTransport::new(move |_, body| {
        seen_c
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(body));
        let payload: Value = serde_json::from_slice(body).unwrap();
        let cands = payload["state"]["candidates"].as_object().unwrap();
        let key = cands
            .iter()
            .find(|(_, v)| v["text"] == "Payment reconciliation")
            .unwrap()
            .0
            .clone();
        let ans = choice_answer(&payload["questions"]["title"], &key);
        Ok(serde_json::to_vec(&json!({"model": MODEL, "answers": {"title": ans}})).unwrap())
    });
    let (items, _) = panes::name_tab(&host, &t, "").unwrap();
    assert_eq!(items[0]["rename"]["name"], json!("Payment reconciliation"));
    let req = seen.lock().unwrap();
    for marker in [
        "SHELL_MARKER",
        "TMUX_MARKER",
        "PASSWORD_MARKER",
        "BEARER_MARKER",
        "EXFILTRATION_MARKER",
        "COMMAND_MARKER",
        "FENCED_MARKER",
        "INDENTED_MARKER",
        "PRIVATE_NAMING_MARKER",
        "OTHER_WINDOW_MARKER",
        "OTHER_EXCERPT_MARKER",
    ] {
        assert!(!req.contains(marker), "leaked {marker}");
    }
}

// =============================================================================================
// 3. Retained history search (test_history_ai.py)

struct HistoryFixture {
    _tmp: TempDir,
    base: PathBuf,
    root: PathBuf,
    host: FakeTmuxHost,
}

impl HistoryFixture {
    fn new() -> Self {
        let tmp = TempDir::new();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("state");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("snapshots"))
            .unwrap();
        let host = FakeTmuxHost::empty(root.clone());
        host.add_simple_pane(0, "@1", "synthetic", "pane 1", "/tmp", false, true, "");
        Self {
            _tmp: tmp,
            base,
            root,
            host,
        }
    }

    fn uid(&self, idx: usize) -> String {
        self.host.state.lock().unwrap().panes[idx]
            .workspace_id
            .clone()
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn archive(&self, name: &str, sources: &[(&str, &[u8])], saved: Value) -> PathBuf {
        let dir = self.root.join("snapshots").join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let mut panes_meta = Vec::new();
        for &(uid, bytes) in sources {
            Self::write_private(&dir.join(format!("{uid}.txt")), bytes);
            panes_meta.push(json!({"id": uid, "label": "synthetic pane"}));
        }
        let meta = json!({
            "version": 1,
            "saved_at": saved,
            "directory": dir.to_string_lossy(),
            "panes": panes_meta,
        });
        Self::write_private(&dir.join("workspace.json"), meta.to_string().as_bytes());
        dir
    }
}

#[test]
fn history_enforces_consent_ambiguity_fs_bounds_numeric_order_utf8_and_locations() {
    // 1. Unshared pane excluded; duplicate live stable ID rejected as ambiguous.
    let f = HistoryFixture::new();
    let uid1 = f.uid(0);
    f.host
        .add_simple_pane(1, "@2", "synthetic", "pane 2", "/tmp", false, false, "");
    let uid2 = f.uid(1);
    f.archive(
        "100",
        &[
            (&uid1, b"needle PUBLIC_ARCHIVE\n"),
            (&uid2, b"needle PRIVATE_ARCHIVE\n"),
        ],
        json!(100),
    );
    let t = FakeTransport::new(score_all_2);
    let (items, _) = history::search(&f.host, &t, "needle").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["search_text"], "needle PUBLIC_ARCHIVE\n");

    // 2. Symlink ancestor, symlink snapshot dir, symlink file, hardlink file, group-writable rejected.
    let f = HistoryFixture::new();
    let uid = f.uid(0);
    let snap = f.archive("100", &[(&uid, b"needle SECRET\n")], json!(100));
    let forbid = FakeTransport::new(|_, _| panic!("Unsafe path reached model"));

    // Symlink ancestor
    let alias = f.base.join("alias");
    std::os::unix::fs::symlink(&f.base, &alias).unwrap();
    let mut alias_host = FakeTmuxHost::empty(alias.join("state"));
    alias_host.state = Mutex::new(f.host.snapshot());
    let (items, _) = history::search(&alias_host, &forbid, "needle").unwrap();
    assert!(items.is_empty());

    // Hardlinked source file
    let hardlink = f.base.join("hardlink.txt");
    std::fs::hard_link(snap.join(format!("{uid}.txt")), &hardlink).unwrap();
    let (items, _) = history::search(&f.host, &forbid, "needle").unwrap();
    assert!(items.is_empty());
    std::fs::remove_file(&hardlink).unwrap();

    // Group-writable source file
    let src_path = snap.join(format!("{uid}.txt"));
    std::fs::set_permissions(&src_path, std::fs::Permissions::from_mode(0o620)).unwrap();
    let (items, _) = history::search(&f.host, &forbid, "needle").unwrap();
    assert!(items.is_empty());
    std::fs::set_permissions(&src_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    // Symlink source file
    let target = f.base.join("moved.txt");
    std::fs::rename(&src_path, &target).unwrap();
    std::os::unix::fs::symlink(&target, &src_path).unwrap();
    let (items, _) = history::search(&f.host, &forbid, "needle").unwrap();
    assert!(items.is_empty());
    std::fs::remove_file(&src_path).unwrap();
    std::fs::rename(&target, &src_path).unwrap();

    // 3. Boolean version / overflow timestamp / escaped directory rejected.
    let meta_path = snap.join("workspace.json");
    let orig_meta = std::fs::read(&meta_path).unwrap();
    for bad_meta in [
        json!({"version": true, "saved_at": 100, "directory": snap.to_string_lossy(), "panes": []}),
        json!({"version": 1, "saved_at": 1e100_f64, "directory": snap.to_string_lossy(), "panes": []}),
        json!({"version": 1, "saved_at": 100, "directory": f.base.to_string_lossy(), "panes": []}),
    ] {
        HistoryFixture::write_private(&meta_path, bad_meta.to_string().as_bytes());
        let (items, _) = history::search(&f.host, &forbid, "needle").unwrap();
        assert!(items.is_empty());
    }
    HistoryFixture::write_private(&meta_path, &orig_meta);

    // 4. Numeric order keeps newest 3 (100, 10, 9), drops 2.
    let f = HistoryFixture::new();
    let uid = f.uid(0);
    for n in ["9", "2", "100", "10"] {
        f.archive(
            n,
            &[(&uid, format!("needle snapshot {n}\n").as_bytes())],
            json!(n.parse::<i64>().unwrap()),
        );
    }
    let (items, _) = history::search(&f.host, &t, "needle").unwrap();
    let texts: Vec<&str> = items
        .iter()
        .map(|i| i["search_text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            "needle snapshot 100\n",
            "needle snapshot 10\n",
            "needle snapshot 9\n"
        ]
    );

    // 5. Corrupt UTF-8 dropped; truncated multibyte suffix cleanly trimmed without U+FFFD.
    let f = HistoryFixture::new();
    let uid1 = f.uid(0);
    f.host
        .add_simple_pane(1, "@2", "synthetic", "pane 2", "/tmp", false, true, "");
    let uid2 = f.uid(1);
    let mut cut_bytes = b"needle valid prefix\n".to_vec();
    cut_bytes.resize(history::SOURCE_BYTES - 1, b'x');
    cut_bytes.extend_from_slice("界\nAFTER_CAP\n".as_bytes());
    f.archive(
        "100",
        &[(&uid1, b"needle CORRUPT\xff\n"), (&uid2, &cut_bytes)],
        json!(100),
    );
    let (items, notice) = history::search(&f.host, &t, "needle").unwrap();
    assert!(items.iter().any(|i| {
        i["search_text"]
            .as_str()
            .unwrap()
            .contains("needle valid prefix")
    }));
    let serialized = serde_json::to_string(&items).unwrap();
    assert!(!serialized.contains("CORRUPT"));
    assert!(!serialized.contains("AFTER_CAP"));
    assert!(!serialized.contains('\u{fffd}'));
    assert!(notice.to_lowercase().contains("limited"));

    // 6. Redaction does not shift raw character locations.
    let f = HistoryFixture::new();
    let uid = f.uid(0);
    let mut first = "\x1b[31mneedle colored\x1b[0m\npassword=synthetic-private-value\n".to_string();
    first.push_str(&"x".repeat(history::CHUNK_CHARS - first.chars().count() - 1));
    first.push('\n');
    let second = "needle exact second quote\n";
    f.archive(
        "100",
        &[(&uid, format!("{first}{second}").as_bytes())],
        json!(100),
    );
    let (items, _) = history::search(&f.host, &t, "needle").unwrap();
    let second_item = items.iter().find(|i| i["search_text"] == second).unwrap();
    assert!(
        second_item["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("line 4, character {}", history::CHUNK_CHARS + 1))
    );
    assert!(
        !serde_json::to_string(&items)
            .unwrap()
            .contains("synthetic-private-value")
    );
}

// =============================================================================================
// 4. Suggest tests (topics + installed SKILL.md + local fallback)

#[test]
fn suggest_scans_topics_and_skills_verifies_excerpts_and_falls_back_cleanly() {
    let tmp = TempDir::new();
    let skill_dir = tmp.path().join("skills/demo-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: Helps deploy invoices cleanly.\n---\n\nRun checks before invoice deployment.\n",
    )
    .unwrap();
    let mut host = FakeTmuxHost::empty(tmp.path().to_path_buf());
    host.skill_roots = vec![tmp.path().join("skills")];

    let t = FakeTransport::new(|_, body| {
        let payload: Value = serde_json::from_slice(body).unwrap();
        let questions = payload["questions"].as_object().unwrap();
        let mut answers = Map::new();
        for (k, q) in questions {
            if q["type"] == "score" {
                let idx: usize = k.strip_prefix("candidate_").unwrap().parse().unwrap();
                let cand = &payload["state"]["candidates"][idx];
                let hit = cand["title"].as_str().unwrap_or("").contains("demo-skill");
                answers.insert(
                    k.clone(),
                    if hit {
                        json!({"type": "score", "score": 2.0, "confidence": 0.95, "probabilities": {"0": 0.0, "1": 0.0, "2": 1.0}})
                    } else {
                        json!({"type": "score", "score": 0.0, "confidence": 0.95, "probabilities": {"0": 1.0, "1": 0.0, "2": 0.0}})
                    },
                );
            } else {
                answers.insert(k.clone(), choice_answer(q, "excerpt_0"));
            }
        }
        Ok(serde_json::to_vec(&json!({"model": MODEL, "answers": answers})).unwrap())
    });

    let (items, notice) = suggest::suggest(&host, &t, "deploy invoices");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "/demo-skill");
    assert!(
        items[0]["detail"]
            .as_str()
            .unwrap()
            .contains("Run checks before invoice deployment.")
    );
    assert!(notice.contains("Suggestions only; nothing was executed."));

    // Service failure returns local catalog marked LOCAL CATALOG.
    let failing = FakeTransport::err(AiError::Other("offline".into()));
    let (items, notice) = suggest::suggest(&host, &failing, "deploy invoices");
    assert!(!items.is_empty());
    assert!(items.iter().all(|i| i["relevance"] == "local"));
    assert!(
        items[0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("LOCAL CATALOG — NOT AN AI RECOMMENDATION")
    );
    assert!(notice.contains("Local catalog only"));
}

// =============================================================================================
// 5. Action safety (test_ai_actions.py)

#[test]
fn live_and_parked_navigation_validate_identity_before_and_during_confirmation() {
    let host = FakeTmuxHost::action_fixture();
    let target = host.pane_value("%4");
    assert!(ai::navigate(&host, &target, &FakeUi::forbid_prompt()).unwrap());
    let focus = host.state.lock().unwrap().focus.clone();
    assert_eq!(
        focus,
        Focus {
            session: "other".into(),
            window: "@4".into(),
            pane: "%4".into()
        }
    );

    // Stale live pane is never selected.
    for change in ["deleted", "reused", "cwd", "process", "server-restarted"] {
        let host = FakeTmuxHost::action_fixture();
        let selected = host.pane_value("%4");
        host.change("%4", change);
        let before = host.snapshot();
        assert!(!ai::navigate(&host, &selected, &FakeUi::forbid_prompt()).unwrap());
        assert_eq!(host.snapshot(), before);
    }

    // Parked cancel keeps all panes parked; confirmed ' Y ' moves exact selected pane (%2, not %3).
    let host = FakeTmuxHost::action_fixture();
    for ans in ["", "n", "yes", "SHARE"] {
        let before = host.snapshot();
        assert!(!ai::navigate(&host, &host.pane_value("%2"), &FakeUi::answers(&[ans])).unwrap());
        assert_eq!(host.snapshot(), before);
    }
    assert!(ai::navigate(&host, &host.pane_value("%2"), &FakeUi::answers(&[" Y "])).unwrap());
    assert_eq!(host.pane_value("%2")["window_id"], "@1");
    assert_eq!(host.pane_value("%2")["parked"], json!(false));
    assert_eq!(host.pane_value("%3")["parked"], json!(true));

    // Revalidates source and destination during confirmation.
    for change in [
        "deleted",
        "reused",
        "cwd",
        "process",
        "moved",
        "server-restarted",
    ] {
        let host = Arc::new(FakeTmuxHost::action_fixture());
        let selected = host.pane_value("%2");
        let host_c = Arc::clone(&host);
        let after = Arc::new(Mutex::new(None));
        let after_c = Arc::clone(&after);
        let ch = change.to_string();
        let ui = FakeUi::custom(true, move |_| {
            host_c.change("%2", &ch);
            *after_c.lock().unwrap() = Some(host_c.snapshot());
            Ok("y".into())
        });
        assert!(!ai::navigate(host.as_ref(), &selected, &ui).unwrap());
        assert_eq!(host.snapshot(), after.lock().unwrap().clone().unwrap());
    }
}

#[test]
fn rename_tab_and_sharing_enforce_confirmation_and_revalidation() {
    let host = FakeTmuxHost::action_fixture();
    let proposal = host.proposal("@1");
    for ans in ["", "n", "yes"] {
        let before = host.snapshot();
        assert!(!ai::rename_tab(&host, &proposal, &FakeUi::answers(&[ans])).unwrap());
        assert_eq!(host.snapshot(), before);
    }
    assert!(ai::rename_tab(&host, &proposal, &FakeUi::answers(&["y"])).unwrap());
    assert_eq!(host.state.lock().unwrap().windows[0].1, "Billing review");

    // Manual rename during confirmation wins.
    let host = Arc::new(FakeTmuxHost::action_fixture());
    let proposal = host.proposal("@1");
    let host_c = Arc::clone(&host);
    let ui = FakeUi::custom(true, move |_| {
        host_c.state.lock().unwrap().windows[0].1 = "Manual name".into();
        Ok("y".into())
    });
    assert!(!ai::rename_tab(host.as_ref(), &proposal, &ui).unwrap());
    assert_eq!(host.state.lock().unwrap().windows[0].1, "Manual name");

    // Sharing requires exact SHARE interactively, --allow non-interactively, and rejects unstable ID.
    let host = FakeTmuxHost::action_fixture();
    for ans in ["", "y", "yes", "share"] {
        let before = host.snapshot();
        ai::sharing(&host, Some("%1"), false, false, &FakeUi::answers(&[ans])).unwrap();
        assert_eq!(host.snapshot(), before);
    }
    let non_tty = FakeUi::custom(false, |_| panic!("must not prompt"));
    assert!(ai::sharing(&host, Some("%1"), false, false, &non_tty).is_err());
    ai::sharing(&host, Some("%1"), true, false, &non_tty).unwrap();
    assert_eq!(host.pane_value("%1")["shared"], json!(true));
    assert_eq!(host.pane_value("%2")["shared"], json!(false));

    // Toggle revokes without prompting.
    ai::sharing(&host, Some("%1"), false, false, &FakeUi::forbid_prompt()).unwrap();
    assert_eq!(host.pane_value("%1")["shared"], json!(false));

    // Missing pane or blank stable ID rejected even with --allow.
    host.state.lock().unwrap().panes[0].workspace_id.clear();
    assert!(ai::sharing(&host, Some("%1"), true, false, &non_tty).is_err());
    assert!(ai::sharing(&host, Some("%404"), true, false, &non_tty).is_err());
}

#[test]
fn menu_check_waits_for_dismissal_and_cli_selection_requires_g_to_navigate() {
    // Menu check waits for Enter dismissal; direct `check` exits immediately.
    let host = Arc::new(FakeTmuxHost::action_fixture());
    let transport = FakeTransport::answers(json!({"check": {"type": "noul", "noul": 1.0}}));
    let waited = Arc::new((Mutex::new(false), Condvar::new()));
    let waited_c = Arc::clone(&waited);
    let step = Arc::new(Mutex::new(0usize));
    let ui = FakeUi::custom(true, move |_| {
        let mut s = step.lock().unwrap();
        *s += 1;
        if *s == 1 {
            Ok("7".into()) // pick `check` from menu
        } else {
            let (lock, cvar) = &*waited_c;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            Ok(String::new())
        }
    });
    assert_eq!(
        ai::main_with(host.as_ref(), &transport, &ui, &[]).unwrap(),
        0
    );
    assert!(*waited.0.lock().unwrap());

    // Direct `check` does not prompt.
    assert_eq!(
        ai::main_with(
            host.as_ref(),
            &transport,
            &FakeUi::forbid_prompt(),
            &["check".into()]
        )
        .unwrap(),
        0
    );

    // Result selection alone cannot navigate; only 'g' navigates.
    for (answers, expected) in [
        (&["4", "q"][..], "%1"),
        (&["4", "", ""][..], "%1"),
        (&["4", "yes", ""][..], "%1"),
        (&["4", "g"][..], "%4"),
    ] {
        let host = FakeTmuxHost::action_fixture();
        let before = host.snapshot();
        let t = FakeTransport::new(score_all_2);
        let ui = FakeUi::answers(answers);
        ai::main_with(&host, &t, &ui, &["find".into(), "review".into()]).unwrap();
        assert_eq!(host.state.lock().unwrap().focus.pane, expected);
        if expected == "%1" {
            assert_eq!(host.snapshot(), before);
        }
    }
}
