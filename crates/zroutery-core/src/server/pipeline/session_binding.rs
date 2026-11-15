//! Zroutery-owned upstream session renewal. No prompts or credentials are stored.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderName, HeaderValue};
use serde_json::{json, Value};

use crate::config::ClientIdentity;
use crate::ir::{ChatRequest, ReplyHeaders};
use crate::protocol::SseFrame;
use crate::router::Candidate;

const MAX_ALIASES: usize = 4096;
const MAX_IDLE: Duration = Duration::from_secs(24 * 60 * 60);
const SESSION_HEADERS: [&str; 3] = ["session_id", "x-session-id", "x-opencode-session"];
const CLAUDE_SESSION_HEADER: &str = "x-claude-code-session-id";
const THREAD_IDLE: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Scope {
    provider: String,
    endpoint: String,
    model: String,
    key_ref: String,
    account: Option<String>,
    key: Option<String>,
    profile: String,
    binding_version: Option<String>,
}

impl Scope {
    fn of(candidate: &Candidate, client: &ClientIdentity) -> Self {
        Self {
            provider: candidate.provider.id.clone(),
            endpoint: candidate.provider.chat_url(),
            model: candidate.entry.upstream_model.clone(),
            key_ref: candidate.provider.key_ref.clone(),
            account: candidate
                .credential
                .as_ref()
                .map(|key| key.account_id.clone()),
            key: candidate.credential.as_ref().map(|key| key.key_id.clone()),
            profile: client.profile.as_str().to_string(),
            binding_version: candidate.credential.as_ref().and_then(|credential| {
                candidate
                    .provider
                    .accounts
                    .iter()
                    .find(|account| account.account_id == credential.account_id)
                    .and_then(|account| account.binding.as_ref())
                    .map(|binding| binding.version())
            }),
        }
    }
}

struct Binding {
    active: String,
    seen: Instant,
}

struct Inner {
    aliases: HashMap<(Scope, String), Arc<Mutex<Binding>>>,
    pruned: Instant,
    threads: HashMap<String, ThreadBinding>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            aliases: HashMap::new(),
            pruned: Instant::now(),
            threads: HashMap::new(),
        }
    }
}

#[derive(Default)]
pub(in crate::server) struct UpstreamSessions(Mutex<Inner>);

pub(super) struct BindingAttempt {
    scope: Scope,
    signal: Option<String>,
    used: Option<String>,
    refreshed: bool,
}

#[derive(Clone)]
struct ThreadBinding {
    scope: Scope,
    seen: Instant,
    conflicted: bool,
}

impl UpstreamSessions {
    pub(super) fn thread_registry(
        &self,
        req: &ChatRequest,
        registry: &crate::registry::Registry,
    ) -> crate::error::Result<Arc<crate::registry::Registry>> {
        let Some(binding) = self.thread_binding(req)? else {
            return Ok(Arc::new(registry.clone()));
        };
        let mut config = (*registry.snapshot()).clone();
        for model in &mut config.models {
            if model.provider_id != binding.scope.provider
                || model.upstream_model != binding.scope.model
            {
                model.enabled = false;
            }
        }
        if let Some(provider) = config
            .providers
            .iter_mut()
            .find(|provider| provider.id == binding.scope.provider)
        {
            if let (Some(account_id), Some(key_id)) = (&binding.scope.account, &binding.scope.key) {
                for account in &mut provider.accounts {
                    if &account.account_id != account_id {
                        account.enabled = false;
                    }
                    for key in &mut account.inference_keys {
                        if &key.id != key_id {
                            key.enabled = false;
                        }
                    }
                }
            }
        }
        Ok(Arc::new(crate::registry::Registry::new(Arc::new(config))))
    }

    fn thread_binding(&self, req: &ChatRequest) -> crate::error::Result<Option<ThreadBinding>> {
        let Some(thread) = req
            .anthropic_body
            .as_ref()
            .and_then(|body| body.get("thread"))
            .filter(|value| !value.is_null())
        else {
            return Ok(None);
        };
        let kind = thread.get("type").and_then(Value::as_str);
        if kind != Some("continue") {
            return Ok(None);
        }
        let previous = thread
            .get("previous_message_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or_else(|| {
                crate::error::Error::invalid(
                    "thread.previous_message_id is required for continuation",
                )
            })?;
        crate::sync::lock(&self.0).threads.get(previous).cloned()
            .filter(|binding| binding.seen.elapsed() < THREAD_IDLE && !binding.conflicted)
            .map(Some)
            .ok_or_else(|| crate::error::Error::UnknownModel("message thread expired or unbound; replay the full history with thread.type=create".into()))
    }

    pub(super) fn thread_plan(
        &self,
        req: &ChatRequest,
        plan: Vec<Candidate>,
    ) -> crate::error::Result<Vec<Candidate>> {
        let Some(binding) = self.thread_binding(req)? else {
            return Ok(plan);
        };
        let plan: Vec<_> = plan
            .into_iter()
            .filter(|candidate| {
                Scope::of(
                    candidate,
                    &super::client::client_for(req, &candidate.provider),
                ) == binding.scope
            })
            .collect();
        if plan.is_empty() {
            return Err(crate::error::Error::UnknownModel("message thread binding changed or is unavailable; replay the full history with thread.type=create".into()));
        }
        Ok(plan)
    }

    pub(super) fn validate_thread_send(
        &self,
        req: &ChatRequest,
        candidate: &Candidate,
    ) -> crate::error::Result<()> {
        self.thread_plan(req, vec![candidate.clone()]).map(|_| ())
    }

    pub(super) fn remember_thread(&self, id: &str, candidate: &Candidate, client: &ClientIdentity) {
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return;
        }
        let mut inner = crate::sync::lock(&self.0);
        inner
            .threads
            .retain(|_, binding| binding.seen.elapsed() < THREAD_IDLE);
        if inner.threads.len() >= MAX_ALIASES {
            if let Some(oldest) = inner
                .threads
                .iter()
                .min_by_key(|(_, binding)| binding.seen)
                .map(|(id, _)| id.clone())
            {
                inner.threads.remove(&oldest);
            }
        }
        // Namespace IDs by the actual execution, never overwrite another
        // credential's thread when a relay reuses a message identifier.
        let scope = Scope::of(candidate, client);
        if let Some(binding) = inner.threads.get_mut(id) {
            if binding.conflicted || binding.scope != scope {
                // Keep a bounded, expiring tombstone. Removing the binding
                // would let the next colliding create resurrect it on a sibling
                // credential and misroute an older client's continuation.
                binding.conflicted = true;
                binding.seen = Instant::now();
                return;
            }
        }
        inner.threads.insert(
            id.into(),
            ThreadBinding {
                scope,
                seen: Instant::now(),
                conflicted: false,
            },
        );
    }

    pub(super) fn resolve(
        &self,
        candidate: &Candidate,
        body: &mut Value,
        client: &mut ClientIdentity,
    ) -> BindingAttempt {
        let scope = Scope::of(candidate, client);
        let signal = client
            .session
            .clone()
            .or_else(|| body_signal(body))
            .and_then(|signal| valid_signal(&signal).map(str::to_string));
        let threaded = body.get("thread").is_some_and(|value| !value.is_null());
        let active = signal.as_ref().filter(|_| !threaded).and_then(|signal| {
            let mut inner = crate::sync::lock(&self.0);
            prune(&mut inner);
            inner
                .aliases
                .get(&(scope.clone(), signal.clone()))
                .map(|binding| {
                    let mut binding = crate::sync::lock(binding);
                    binding.seen = Instant::now();
                    binding.active.clone()
                })
        });
        client.reply_headers.clear_session_id();
        if let Some(active) = &active {
            apply_id(body, client, active, signal.as_deref());
            self.remap_user_alias(&scope, body, active);
        }
        BindingAttempt {
            scope,
            used: active.or_else(|| signal.clone()),
            signal,
            refreshed: false,
        }
    }

    fn renew(&self, attempt: &BindingAttempt, client: &ClientIdentity) -> String {
        let mut inner = crate::sync::lock(&self.0);
        prune(&mut inner);
        let binding = attempt.signal.as_ref().and_then(|signal| {
            inner
                .aliases
                .get(&(attempt.scope.clone(), signal.clone()))
                .cloned()
        });
        let binding = binding.unwrap_or_else(|| {
            Arc::new(Mutex::new(Binding {
                active: attempt.used.clone().unwrap_or_default(),
                seen: Instant::now(),
            }))
        });
        let id = {
            let mut current = crate::sync::lock(&binding);
            // Another request may already have repaired the same old binding.
            // Reuse its replacement instead of invalidating a working session.
            if current.active.is_empty() || Some(current.active.as_str()) == attempt.used.as_deref()
            {
                current.active = new_id(client);
            }
            current.seen = Instant::now();
            current.active.clone()
        };
        if let Some(signal) = &attempt.signal {
            insert_alias(
                &mut inner,
                (attempt.scope.clone(), signal.clone()),
                binding.clone(),
            );
        }
        insert_alias(&mut inner, (attempt.scope.clone(), id.clone()), binding);
        id
    }

    fn remap_user_alias(&self, scope: &Scope, body: &mut Value, id: &str) {
        let Some(user) = body
            .get("user")
            .and_then(Value::as_str)
            .filter(|user| user.len() <= 1024 && *user != id)
        else {
            return;
        };
        let related = crate::sync::lock(&self.0)
            .aliases
            .get(&(scope.clone(), user.to_string()))
            .is_some_and(|binding| crate::sync::lock(binding).active == id);
        // Only rewrite a user field proven to be an alias of this exact
        // binding. Ordinary account/user identifiers are left untouched.
        if related {
            body["user"] = json!(id);
        }
    }
}

impl BindingAttempt {
    /// One renewed-session replay per candidate, within the caller's retry budget.
    pub(super) fn refresh(
        &mut self,
        sessions: &UpstreamSessions,
        body: &mut Value,
        client: &mut ClientIdentity,
    ) -> bool {
        if self.refreshed || body.get("thread").is_some_and(|value| !value.is_null()) {
            return false;
        }
        self.refreshed = true;
        let id = sessions.renew(self, client);
        apply_id(body, client, &id, self.used.as_deref());
        sessions.remap_user_alias(&self.scope, body, &id);
        self.used = Some(id);
        true
    }
}

fn prune(inner: &mut Inner) {
    if inner.pruned.elapsed() < Duration::from_secs(60) {
        return;
    }
    inner
        .aliases
        .retain(|_, binding| crate::sync::lock(binding).seen.elapsed() < MAX_IDLE);
    inner.pruned = Instant::now();
}

fn insert_alias(inner: &mut Inner, key: (Scope, String), binding: Arc<Mutex<Binding>>) {
    if !inner.aliases.contains_key(&key) && inner.aliases.len() >= MAX_ALIASES {
        let oldest = inner
            .aliases
            .iter()
            .min_by_key(|(_, binding)| crate::sync::lock(binding).seen)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            inner.aliases.remove(&oldest);
        }
    }
    inner.aliases.insert(key, binding);
}

fn new_id(client: &ClientIdentity) -> String {
    #[cfg(feature = "opencode")]
    if client.profile == crate::config::ProviderClientProfile::OpenCodeCli {
        return crate::opencode::session_id();
    }
    #[cfg(not(feature = "opencode"))]
    let _ = client;
    uuid::Uuid::new_v4().to_string()
}

fn valid_signal(signal: &str) -> Option<&str> {
    let signal = signal.trim();
    (!signal.is_empty() && signal.len() <= 1024).then_some(signal)
}

fn body_signal(body: &Value) -> Option<String> {
    body.get("session_id")
        .and_then(Value::as_str)
        .or_else(|| body.get("prompt_cache_key").and_then(Value::as_str))
        .or_else(|| body.pointer("/metadata/session_id").and_then(Value::as_str))
        .and_then(valid_signal)
        .map(str::to_string)
        .or_else(|| {
            body.pointer("/metadata/user_id")
                .and_then(Value::as_str)
                .and_then(claude_user_session)
        })
        .or_else(|| {
            body.get("user")
                .and_then(Value::as_str)
                .and_then(claude_user_session)
        })
}

pub(super) fn session_signal(req: &ChatRequest) -> Option<String> {
    [
        "x-zroutery-session-id",
        "session_id",
        "x-session-id",
        "x-opencode-session",
        CLAUDE_SESSION_HEADER,
    ]
    .iter()
    .find_map(|name| {
        req.client_headers
            .get(*name)?
            .to_str()
            .ok()
            .and_then(valid_signal)
            .map(str::to_string)
    })
    .or_else(|| req.responses_body.as_ref().and_then(body_signal))
    .or_else(|| req.anthropic_body.as_ref().and_then(body_signal))
    .or_else(|| {
        req.passthrough
            .get("session_id")
            .and_then(Value::as_str)
            .and_then(valid_signal)
            .map(str::to_string)
    })
    .or_else(|| {
        req.passthrough
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .and_then(valid_signal)
            .map(str::to_string)
    })
    .or_else(|| req.metadata_user.as_deref().and_then(claude_user_session))
    .or_else(|| {
        req.metadata_user
            .as_deref()
            .filter(|id| id.starts_with("ses_"))
            .and_then(valid_signal)
            .map(str::to_string)
    })
}

fn apply_id(body: &mut Value, client: &mut ClientIdentity, id: &str, previous: Option<&str>) {
    let claude = client.profile == crate::config::ProviderClientProfile::ClaudeCode
        || client
            .passthrough
            .iter()
            .any(|(name, _)| name.as_str() == CLAUDE_SESSION_HEADER)
        || body
            .pointer("/metadata/user_id")
            .and_then(Value::as_str)
            .and_then(claude_user_session)
            .is_some()
        || body
            .get("user")
            .and_then(Value::as_str)
            .and_then(claude_user_session)
            .is_some();
    let value = HeaderValue::from_str(id).expect("generated session id is a valid header");
    for name in SESSION_HEADERS
        .into_iter()
        .chain(claude.then_some(CLAUDE_SESSION_HEADER))
    {
        let name = HeaderName::from_static(name);
        if let Some((_, old)) = client.passthrough.iter_mut().find(|(key, _)| key == name) {
            *old = value.clone();
        } else {
            client.passthrough.push((name, value.clone()));
        }
    }
    client.session = Some(id.to_string());
    client.reply_headers.set_session_id(value, claude);
    for field in ["session_id", "prompt_cache_key"] {
        if body.get(field).is_some_and(Value::is_string) {
            body[field] = json!(id);
        }
    }
    if body
        .pointer("/metadata/session_id")
        .is_some_and(Value::is_string)
    {
        body["metadata"]["session_id"] = json!(id);
    }
    if body
        .get("user")
        .and_then(Value::as_str)
        .is_some_and(|user| Some(user) == previous)
    {
        body["user"] = json!(id);
    }
    if let Some(user) = body
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
        .and_then(|user| renewed_claude_user(user, id))
    {
        body["metadata"]["user_id"] = json!(user);
    }
    if let Some(user) = body
        .get("user")
        .and_then(Value::as_str)
        .and_then(|user| renewed_claude_user(user, id))
    {
        body["user"] = json!(user);
    }
}

/// Only explicit session fields in Claude Code's known user-id envelopes are
/// decoded. Ordinary account identifiers must never become session aliases.
fn claude_user_session(user: &str) -> Option<String> {
    if user.len() > 16 * 1024 {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(user) {
        return value
            .get("session_id")
            .and_then(Value::as_str)
            .and_then(valid_signal)
            .map(str::to_string);
    }
    legacy_claude_session(user).map(|(_, session)| session.to_string())
}

fn legacy_claude_session(user: &str) -> Option<(&str, &str)> {
    let (prefix, session) = user.rsplit_once("_session_")?;
    let (device, _) = prefix.strip_prefix("user_")?.split_once("_account_")?;
    if device.is_empty() || uuid::Uuid::parse_str(session).is_err() {
        return None;
    }
    Some((prefix, session))
}

fn renewed_claude_user(user: &str, id: &str) -> Option<String> {
    let session = claude_user_session(user)?;
    if session == id {
        return Some(user.to_string());
    }
    if let Ok(mut value) = serde_json::from_str::<Value>(user) {
        value["session_id"] = json!(id);
        return Some(value.to_string());
    }
    let (prefix, _) = legacy_claude_session(user)?;
    Some(format!("{prefix}_session_{id}"))
}

pub(super) fn annotate_response(body: &mut Value, reply: &ReplyHeaders) {
    let Some(id) = reply.session_id() else {
        return;
    };
    let Some(body) = body.as_object_mut() else {
        return;
    };
    let metadata = body.entry("metadata").or_insert_with(|| json!({}));
    if metadata.is_null() {
        *metadata = json!({});
    }
    if let Some(metadata) = metadata.as_object_mut() {
        metadata.insert("zroutery_session_id".into(), json!(id));
    }
}

pub(super) fn annotate_frames(frames: &mut [SseFrame], reply: &ReplyHeaders) {
    if reply.session_id().is_none() {
        return;
    }
    for frame in frames {
        if let Ok(mut body) = frame.json() {
            if let Some(response) = body.get_mut("response") {
                annotate_response(response, reply);
                frame.data = body.to_string();
            }
        }
    }
}
