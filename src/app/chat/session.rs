//! Conversation state: per-character history, one in-flight remote request,
//! and the local-line fallback.
//!
//! The session never blocks a frame. A remote request runs on its own thread
//! and reports through a channel that [`ChatSession::poll`] drains from the
//! UI tick; an unavailable or failed endpoint falls back to a local line so
//! the character always answers something.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use super::net;
use super::{ChatRequest, Message, Role, clamp_message, parse_reply, system_prompt};
use crate::i18n::{self, LocalLines};
use crate::preferences::{ChatSettings, Resolved};
/// Most messages kept per character (a user turn plus its reply count as two).
const MAX_HISTORY: usize = 64;

/// One finished remote attempt.
struct Completion {
    instance: u64,
    result: Result<String, String>,
}

/// What a completed send produced, for the UI to display as status text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub instance: u64,
    pub text: String,
    pub remote: bool,
}

pub struct ChatSession {
    settings: ChatSettings,
    language: Resolved,
    history: HashMap<u64, Vec<Message>>,
    receiver: Option<Receiver<Completion>>,
    /// The character waiting on the in-flight request.
    inflight: Option<u64>,
    last_error: Option<String>,
    /// Deterministic index into the local reply bank.
    local_counter: usize,
}

impl ChatSession {
    pub fn new(settings: ChatSettings, language: Resolved) -> Self {
        Self {
            settings,
            language,
            history: HashMap::new(),
            receiver: None,
            inflight: None,
            last_error: None,
            local_counter: 0,
        }
    }

    /// Applies settings changed by the settings page while the pet runs.
    pub fn configure(&mut self, settings: ChatSettings, language: Resolved) {
        self.settings = settings;
        self.language = language;
    }

    /// The resolved language the character answers in.
    pub fn language(&self) -> Resolved {
        self.language
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn clear_error(&mut self) {
        self.last_error = None;
    }

    /// The visible conversation with one character.
    pub fn history(&self, instance: u64) -> &[Message] {
        self.history
            .get(&instance)
            .map_or(&[], |messages| messages.as_slice())
    }

    /// Whether a remote request is still in flight, and for whom.
    pub fn inflight(&self) -> Option<u64> {
        self.inflight
    }

    /// Whether a remote request can be attempted with the current settings.
    pub fn remote_ready(&self) -> bool {
        net::ready(&self.settings).is_ok()
    }

    pub fn clear_history(&mut self, instance: u64) {
        self.history.remove(&instance);
    }

    /// Drops history for characters that no longer exist.
    pub fn retain_instances(&mut self, alive: &[u64]) {
        self.history.retain(|id, _| alive.contains(id));
    }

    /// Sends `text` to one character.
    ///
    /// With a usable endpoint the request goes out on a worker thread and the
    /// reply arrives through [`poll`](Self::poll). Otherwise, or when the
    /// endpoint is not configured, a local line is appended immediately. The
    /// return value is the line to show right away, if any.
    pub fn send(&mut self, instance: u64, character: &str, text: &str) -> Option<Reply> {
        let text = clamp_message(text);
        if text.is_empty() {
            return None;
        }
        let endpoint = match net::ready(&self.settings) {
            Ok(endpoint) => {
                if self.inflight.is_some() {
                    // The new turn is refused whole: answering it locally
                    // would put a line in the history the remote request
                    // never saw.
                    self.last_error = Some("The character is still answering".into());
                    return None;
                }
                Some(endpoint)
            }
            Err(error) => {
                // Not configured is the normal local mode, not an error to
                // shout about; an invalid URL is worth reporting.
                if self.settings.enabled {
                    self.last_error = Some(error);
                }
                None
            }
        };
        self.push(instance, Role::User, text.clone());
        let Some(endpoint) = endpoint else {
            return Some(self.local_reply(instance));
        };
        let system = system_prompt(character, self.language);
        let history = self.history.get(&instance).cloned().unwrap_or_default();
        // The history now already contains the new user turn; build from the
        // turn before it so the request does not duplicate the message.
        let context = &history[..history.len().saturating_sub(1)];
        let request = ChatRequest::build(&self.settings, &system, context, &text);
        let body = request.to_json();
        let settings = self.settings.clone();
        let (sender, receiver) = mpsc::channel();
        let spawn = thread::Builder::new()
            .name("delicium-chat".into())
            .spawn(move || {
                let result = net::post_chat(&settings, &endpoint, &body)
                    .and_then(|body| parse_reply(&body).map_err(|error| error.to_string()));
                let _ = sender.send(Completion { instance, result });
            });
        match spawn {
            Ok(_) => {
                self.receiver = Some(receiver);
                self.inflight = Some(instance);
                None
            }
            Err(error) => {
                self.last_error = Some(format!("Cannot start the chat worker: {error}"));
                Some(self.local_reply(instance))
            }
        }
    }

    /// Drains a finished remote request, appending its reply or the local
    /// fallback when it failed. `None` while nothing has finished.
    pub fn poll(&mut self) -> Option<Reply> {
        let receiver = self.receiver.as_ref()?;
        let completion = match receiver.try_recv() {
            Ok(completion) => completion,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.receiver = None;
                let instance = self.inflight.take()?;
                self.last_error = Some("The chat worker stopped unexpectedly".into());
                return Some(self.local_reply(instance));
            }
        };
        self.receiver = None;
        self.inflight = None;
        Some(match completion.result {
            Ok(text) => {
                self.last_error = None;
                self.push(completion.instance, Role::Assistant, text.clone());
                Reply {
                    instance: completion.instance,
                    text,
                    remote: true,
                }
            }
            Err(error) => {
                self.last_error = Some(error);
                self.local_reply(completion.instance)
            }
        })
    }

    /// Records a character-to-character line for both participants and
    /// returns the two texts, so the controls can show what they said.
    pub fn social(&mut self, first: u64, second: u64, play: bool) -> Option<(String, String)> {
        let lines: &LocalLines = i18n::local_lines(self.language);
        let bank = if play { lines.play } else { lines.greeting };
        let first_line = bank[self.local_counter % bank.len()].to_owned();
        self.local_counter = self.local_counter.wrapping_add(1);
        let second_line = bank[self.local_counter % bank.len()].to_owned();
        self.local_counter = self.local_counter.wrapping_add(1);
        self.push(first, Role::Assistant, first_line.clone());
        self.push(second, Role::Assistant, second_line.clone());
        Some((first_line, second_line))
    }

    fn local_reply(&mut self, instance: u64) -> Reply {
        let lines: &LocalLines = i18n::local_lines(self.language);
        let bank = lines.reply;
        let text = bank[self.local_counter % bank.len()].to_owned();
        self.local_counter = self.local_counter.wrapping_add(1);
        self.push(instance, Role::Assistant, text.clone());
        Reply {
            instance,
            text,
            remote: false,
        }
    }

    fn push(&mut self, instance: u64, role: Role, content: String) {
        let history = self.history.entry(instance).or_default();
        history.push(Message { role, content });
        if history.len() > MAX_HISTORY {
            let excess = history.len() - MAX_HISTORY;
            history.drain(..excess);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::ChatSettings;

    fn local_session() -> ChatSession {
        ChatSession::new(ChatSettings::default(), Resolved::En)
    }

    fn configured(base_url: &str) -> ChatSettings {
        ChatSettings {
            enabled: true,
            base_url: base_url.into(),
            model: "m".into(),
            api_key: String::new(),
        }
    }

    #[test]
    fn an_unconfigured_session_answers_locally_and_remembers_the_turn() {
        let mut session = local_session();
        let reply = session.send(1, "Eous", "hello").unwrap();
        assert!(!reply.remote);
        assert!(!reply.text.is_empty());
        let history = session.history(1);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[0].content, "hello");
        assert_eq!(history[1].role, Role::Assistant);
        // Nothing is reported as a failure: local mode is expected.
        assert!(session.last_error().is_none());
        assert!(session.inflight().is_none());
    }

    #[test]
    fn blank_messages_are_ignored() {
        let mut session = local_session();
        assert!(session.send(1, "Eous", "   ").is_none());
        assert!(session.history(1).is_empty());
    }

    #[test]
    fn chinese_mode_answers_with_chinese_lines() {
        let mut session = ChatSession::new(ChatSettings::default(), Resolved::ZhHans);
        let reply = session.send(1, "Eous", "hi").unwrap();
        assert!(
            reply
                .text
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "{}",
            reply.text
        );
    }

    #[test]
    fn an_insecure_remote_url_is_reported_and_still_answers() {
        let mut session = ChatSession::new(configured("http://example.com/v1"), Resolved::En);
        let reply = session.send(1, "Eous", "hello").unwrap();
        assert!(!reply.remote);
        assert!(
            session
                .last_error()
                .is_some_and(|error| error.contains("https"))
        );
    }

    #[test]
    fn a_duplicate_send_while_busy_is_refused_without_losing_the_queue() {
        let mut session = ChatSession::new(configured("http://127.0.0.1:9/v1"), Resolved::En);
        session.inflight = Some(1);
        let reply = session.send(2, "Eous", "hello");
        assert!(reply.is_none());
        assert_eq!(session.inflight(), Some(1));
        assert!(session.history(2).is_empty());
        assert!(
            session
                .last_error()
                .is_some_and(|error| error.contains("still answering"))
        );
    }

    #[test]
    fn character_lines_land_in_both_histories() {
        let mut session = local_session();
        let (first, second) = session.social(3, 4, false).unwrap();
        assert!(!first.is_empty());
        assert!(!second.is_empty());
        assert_eq!(session.history(3).last().unwrap().content, first);
        assert_eq!(session.history(4).last().unwrap().content, second);
        // The play bank differs from the greeting bank, so consecutive
        // events do not repeat the same line forever.
        let (play_first, _) = session.social(3, 4, true).unwrap();
        assert_ne!(play_first, first);
    }

    #[test]
    fn history_is_bounded_and_cleared_with_its_character() {
        let mut session = local_session();
        for index in 0..MAX_HISTORY {
            session.send(1, "Eous", &format!("turn {index}"));
        }
        assert_eq!(session.history(1).len(), MAX_HISTORY);
        session.clear_history(1);
        assert!(session.history(1).is_empty());
        session.send(1, "Eous", "hello");
        session.retain_instances(&[2]);
        assert!(session.history(1).is_empty());
        assert!(session.history(2).is_empty());
    }

    #[test]
    fn completion_bookkeeping_falls_back_and_clears_the_pending_slot() {
        let mut session = local_session();
        session.inflight = Some(5);
        let (sender, receiver) = mpsc::channel();
        session.receiver = Some(receiver);
        sender
            .send(Completion {
                instance: 5,
                result: Err("connection refused".into()),
            })
            .unwrap();
        let reply = session.poll().unwrap();
        assert_eq!(reply.instance, 5);
        assert!(!reply.remote);
        assert_eq!(session.last_error(), Some("connection refused"));
        assert!(session.inflight().is_none());
        assert!(session.poll().is_none());
    }

    #[test]
    fn a_successful_completion_is_appended_without_an_error() {
        let mut session = local_session();
        session.inflight = Some(5);
        let (sender, receiver) = mpsc::channel();
        session.receiver = Some(receiver);
        sender
            .send(Completion {
                instance: 5,
                result: Ok("Hello from afar".into()),
            })
            .unwrap();
        let reply = session.poll().unwrap();
        assert!(reply.remote);
        assert_eq!(reply.text, "Hello from afar");
        assert!(session.last_error().is_none());
        assert_eq!(
            session.history(5).last().unwrap().content,
            "Hello from afar"
        );
    }
}
