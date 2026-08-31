//! Deciding whether a phone may show its screen here.
//!
//! Nothing is shown until a person has said yes to that phone. The rule is
//! simple, but the timing is not: while the question is on screen the phone
//! gets no answer at all, so it gives up after a few seconds and reconnects.
//! The verdict therefore belongs to the *phone*, not to the connection that
//! happened to ask — otherwise the person answers a question whose phone has
//! already walked away, and is asked all over again.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use log::{info, warn};

use crate::config::Config;
use crate::net::{Admission, Decision, Hello};

/// What the person can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Just this once.
    Allow,
    /// And every time from now on.
    Always,
    /// No — and not again until the receiver restarts.
    Decline,
}

/// How long a connection waits for an answer before giving up on this attempt.
/// The phone will be back; the question stays on screen.
const PATIENCE: Duration = Duration::from_secs(300);

/// A phone allowed "just this once" may reconnect inside this window without
/// being asked again — which is exactly what it does after the wait above.
const ONCE_LASTS: Duration = Duration::from_secs(120);

/// Puts the question in front of a person.
pub trait Ask: Send + Sync + 'static {
    /// The answer comes back through [`Consent::record`], not from here: the
    /// person may take a while, and may answer after the phone has retried.
    fn ask(&self, device: &str, id: &str);
}

#[derive(Debug, Clone, Copy)]
enum Verdict {
    /// The question is on screen.
    Asking,
    /// Allowed at this moment, for this session and a little longer.
    Allowed(Instant),
    Declined,
}

/// The gate every phone passes through.
pub struct Consent {
    settings: Mutex<Config>,
    verdicts: Mutex<HashMap<String, Verdict>>,
    answered: Condvar,
    ask: Box<dyn Ask>,
}

impl Consent {
    pub fn new(settings: Config, ask: Box<dyn Ask>) -> Arc<Self> {
        Arc::new(Self {
            settings: Mutex::new(settings),
            verdicts: Mutex::new(HashMap::new()),
            answered: Condvar::new(),
            ask,
        })
    }

    /// The person answered. `name` is what the phone calls itself, kept
    /// alongside the id so the settings file is readable.
    pub fn record(&self, id: &str, name: &str, answer: Answer) {
        {
            let mut verdicts = self.verdicts.lock().unwrap_or_else(|e| e.into_inner());
            verdicts.insert(
                id.to_string(),
                match answer {
                    Answer::Allow | Answer::Always => Verdict::Allowed(Instant::now()),
                    Answer::Decline => Verdict::Declined,
                },
            );
        }
        if answer == Answer::Always {
            let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            settings.allow(id, name);
            if let Err(e) = settings.save() {
                warn!("cannot remember this phone: {e:#}");
            } else {
                info!("{name} ({id}) is allowed from now on");
            }
        }
        self.answered.notify_all();
    }

    /// The phones this receiver already knows, for the settings and the tray.
    pub fn settings(&self) -> Config {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Admission for Consent {
    fn admit(&self, hello: &Hello, _peer: SocketAddr) -> Decision {
        let id = hello.id.trim().to_string();
        if id.is_empty() {
            return Decision::Refuse("the phone did not say who it is".to_string());
        }
        if self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_allowed(&id)
        {
            return Decision::Allow;
        }

        let device = hello.display_name();
        let mut verdicts = self.verdicts.lock().unwrap_or_else(|e| e.into_inner());
        match verdicts.get(&id).copied() {
            Some(Verdict::Declined) => return Decision::Refuse("declined".to_string()),
            Some(Verdict::Allowed(when)) if when.elapsed() < ONCE_LASTS => return Decision::Allow,
            // The one-off permission has gone stale; ask again.
            Some(Verdict::Allowed(_)) | None => {
                verdicts.insert(id.clone(), Verdict::Asking);
                info!("asking whether {device} ({}) may stream", hello.short_id());
                self.ask.ask(&device, &id);
            }
            // Someone is already looking at the question for this phone.
            Some(Verdict::Asking) => return Decision::Ignore,
        }

        // Wait for the person, without holding anyone else up.
        let deadline = Instant::now() + PATIENCE;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // Leave the question on screen: the phone will be back.
                return Decision::Ignore;
            }
            let (guard, _) = self
                .answered
                .wait_timeout(verdicts, remaining)
                .unwrap_or_else(|e| e.into_inner());
            verdicts = guard;
            match verdicts.get(&id).copied() {
                Some(Verdict::Allowed(_)) => return Decision::Allow,
                Some(Verdict::Declined) => return Decision::Refuse("declined".to_string()),
                Some(Verdict::Asking) | None => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::thread;

    /// Stands in for the window: records what was asked.
    struct Recorder(Sender<(String, String)>);

    impl Ask for Recorder {
        fn ask(&self, device: &str, id: &str) {
            let _ = self.0.send((device.to_string(), id.to_string()));
        }
    }

    fn consent() -> (Arc<Consent>, Receiver<(String, String)>) {
        let (tx, rx) = channel();
        (Consent::new(Config::default(), Box::new(Recorder(tx))), rx)
    }

    fn hello(id: &str) -> Hello {
        Hello {
            id: id.to_string(),
            name: "Ira iPhone".to_string(),
            ..Hello::default()
        }
    }

    fn peer() -> SocketAddr {
        "192.168.1.7:51000".parse().unwrap()
    }

    #[test]
    fn a_nameless_phone_is_turned_away() {
        let (consent, _asked) = consent();
        assert!(matches!(
            consent.admit(&hello("  "), peer()),
            Decision::Refuse(_)
        ));
    }

    #[test]
    fn a_known_phone_walks_straight_in() {
        let (tx, _rx) = channel();
        let mut settings = Config::default();
        settings.allow("KNOWN", "Ira iPhone");
        let consent = Consent::new(settings, Box::new(Recorder(tx)));
        assert!(matches!(
            consent.admit(&hello("KNOWN"), peer()),
            Decision::Allow
        ));
    }

    #[test]
    fn an_unknown_phone_waits_for_the_person() {
        let (consent, asked) = consent();
        let waiting = {
            let consent = consent.clone();
            thread::spawn(move || consent.admit(&hello("NEW"), peer()))
        };

        let (device, id) = asked.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(device, "Ira iPhone");
        assert_eq!(id, "NEW");

        consent.record("NEW", "Ira iPhone", Answer::Allow);
        assert!(matches!(waiting.join().unwrap(), Decision::Allow));
    }

    #[test]
    fn the_phone_that_retries_while_we_ask_is_not_asked_about_twice() {
        let (consent, asked) = consent();
        let waiting = {
            let consent = consent.clone();
            thread::spawn(move || consent.admit(&hello("NEW"), peer()))
        };
        asked.recv_timeout(Duration::from_secs(5)).unwrap();

        // The phone gave up waiting and came back: no second question.
        assert!(matches!(
            consent.admit(&hello("NEW"), peer()),
            Decision::Ignore
        ));
        assert!(asked.try_recv().is_err(), "only one question may be asked");

        consent.record("NEW", "Ira iPhone", Answer::Allow);
        assert!(matches!(waiting.join().unwrap(), Decision::Allow));
        // And the reconnect that follows is let in without asking again.
        assert!(matches!(
            consent.admit(&hello("NEW"), peer()),
            Decision::Allow
        ));
        assert!(asked.try_recv().is_err());
    }

    #[test]
    fn always_allow_is_remembered_in_the_settings() {
        let (consent, _asked) = consent();
        consent.record("FOREVER", "Ira iPhone", Answer::Always);
        assert!(consent.settings().is_allowed("FOREVER"));
        assert!(matches!(
            consent.admit(&hello("FOREVER"), peer()),
            Decision::Allow
        ));
    }

    #[test]
    fn a_declined_phone_is_refused_without_asking_again() {
        let (consent, asked) = consent();
        consent.record("NOPE", "Ira iPhone", Answer::Decline);
        match consent.admit(&hello("NOPE"), peer()) {
            Decision::Refuse(reason) => assert_eq!(reason, "declined"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(asked.try_recv().is_err(), "the person is not asked twice");
    }
}
