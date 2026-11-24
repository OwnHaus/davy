//! Scrubbing before ingest records acquire derived fields or reach a queue.
mod input;
mod keys;
mod meter;
mod patterns;
pub mod settings;

pub use input::{Missing, parse};
pub use meter::Meter;

use crate::heap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;

pub(crate) const SENSITIVE: &str = "[Filtered: sensitive]";
const EMAIL: &str = "[Filtered: email]";
const CARD: &str = "[Filtered: card]";
const REQUEST_BODY: &str = "[Filtered: request body]";
/// Keys whose string values identify, group or route a record where they
/// are the item's own fields. Patterns that a version or an id can look
/// like skip them, as a release `1.2.3.4` is not an address and a release
/// `shop@1.2.3` is not an e-mail address.
const STRUCTURAL: &[&str] = &[
    "release",
    "dist",
    "environment",
    "platform",
    "level",
    "logger",
    "event_id",
    "trace_id",
    "span_id",
    "parent_span_id",
    "monitor_slug",
    "check_in_id",
];
/// A session's ids, which release health counts users by.
const SESSION_IDS: &[&str] = &["did", "sid"];

/// What to remove from a project's records. Stored as JSON, which is read
/// leniently so a policy written by another version still applies.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent operator-controlled scrub classes"
)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Policy {
    pub keys: bool,
    pub credentials: bool,
    pub cards: bool,
    pub email: bool,
    pub personal: bool,
    pub ip: bool,
    /// Normalized: lowercase ASCII letters and digits.
    #[serde(
        deserialize_with = "keys::deserialize",
        serialize_with = "keys::serialize"
    )]
    pub extra_keys: HashSet<String>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            keys: true,
            credentials: true,
            cards: true,
            email: false,
            personal: false,
            ip: true,
            extra_keys: HashSet::new(),
        }
    }
}

/// What an SDK item is, which decides which of its fields are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Event,
    /// User feedback, whose top-level fields are about a person.
    Feedback,
    /// A session or session aggregates, whose ids count users.
    Session,
}

/// Where in an item the walk is.
#[derive(Clone, Copy, Default)]
struct Scope {
    /// In a `user` object or feedback, where names and ids are personal.
    identity: bool,
    session: bool,
    place: Place,
}

/// Where an item keeps its own fields, which [`STRUCTURAL`] keys name only
/// there. Under any other key the same names are free text.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Place {
    #[default]
    Nested,
    Root,
    /// An event's `contexts`, whose `trace` holds the trace ids.
    Contexts,
    /// A session aggregates' `aggregates` list, whose entries hold `did`.
    Aggregates,
    /// Fields of the item one level down: `contexts.trace`, a session's
    /// `attrs` and each entry of `aggregates`.
    Fields,
}

impl Scope {
    /// The scope of an item's top level. A log record's attributes are its
    /// top level, as its own fields are flat there.
    fn root(session: bool) -> Self {
        Self {
            identity: false,
            session,
            place: Place::Root,
        }
    }

    /// The scope of the value under `key` in an object in this scope.
    fn enter(self, key: &str) -> Self {
        let place = match (self.place, key) {
            (Place::Root, "contexts") => Place::Contexts,
            (Place::Contexts, "trace") => Place::Fields,
            (Place::Root, "attrs") if self.session => Place::Fields,
            (Place::Root, "aggregates") if self.session => Place::Aggregates,
            _ => Place::Nested,
        };
        let identity = key == "user" || key == "feedback";
        Self {
            identity,
            place,
            ..self
        }
    }

    /// The scope of an entry in a list in this scope.
    fn entry(self) -> Self {
        let place = if self.place == Place::Aggregates {
            Place::Fields
        } else {
            Place::Nested
        };
        Self { place, ..self }
    }

    /// Whether a string under `key` here is a structural field.
    fn structural(self, key: &str) -> bool {
        match self.place {
            Place::Root | Place::Fields => {
                STRUCTURAL.contains(&key) || (self.session && SESSION_IDS.contains(&key))
            }
            Place::Nested | Place::Contexts | Place::Aggregates => false,
        }
    }
}

impl Policy {
    /// What `new` turns off or removes that `self` has, in words.
    pub fn weakened(&self, new: &Self) -> Vec<String> {
        let classes = [
            ("keys", self.keys, new.keys),
            ("credentials", self.credentials, new.credentials),
            ("cards", self.cards, new.cards),
            ("email", self.email, new.email),
            ("personal", self.personal, new.personal),
            ("ip", self.ip, new.ip),
        ];
        let mut lost: Vec<String> = classes
            .iter()
            .filter(|(_, old, new)| *old && !*new)
            .map(|(name, ..)| format!("turns off {name}"))
            .collect();
        let mut removed: Vec<&String> = self.extra_keys.difference(&new.extra_keys).collect();
        removed.sort_unstable();
        lost.extend(removed.iter().map(|key| format!("removes extra key {key}")));
        lost
    }

    /// Scrubs an event without a budget.
    pub fn value(&self, value: &mut Value) {
        self.item(value, Item::Event, &mut Meter::unmetered());
    }

    /// Scrubs an SDK item, paying what it grows with `meter`. A refused
    /// payment stops it part way, and [`Meter::refused`] says so.
    pub fn item(&self, value: &mut Value, item: Item, meter: &mut Meter<'_>) {
        let scope = Scope {
            identity: item == Item::Feedback,
            ..Scope::root(item == Item::Session)
        };
        self.walk(value, scope, meter);
    }

    /// Scrubs free text without a budget.
    pub fn text(&self, text: &mut String) {
        patterns::scrub(self, text, &mut Meter::unmetered());
    }

    /// Scrubs log records, paying what they grow with `meter`.
    pub fn logs(&self, records: &mut [crate::logs::LogRecord], meter: &mut Meter<'_>) {
        for record in records {
            let scrubbed = patterns::scrub(self, &mut record.message, meter)
                && record
                    .service
                    .as_mut()
                    .is_none_or(|service| patterns::scrub(self, service, meter))
                && self.object(&mut record.attributes, Scope::root(false), meter);
            if !scrubbed {
                return;
            }
        }
    }

    /// False when the meter refused to pay.
    fn walk(&self, value: &mut Value, scope: Scope, meter: &mut Meter<'_>) -> bool {
        match value {
            Value::Object(map) => self.object(map, scope, meter),
            Value::Array(values) => {
                if let [Value::String(key), second] = values.as_mut_slice()
                    && let Some(marker) = self.class(key, false)
                {
                    return self.replace(second, marker, meter);
                }
                let entry = scope.entry();
                values
                    .iter_mut()
                    .all(|value| self.walk(value, entry, meter))
            }
            Value::String(text) => patterns::scrub(self, text, meter),
            Value::Number(n) if self.cards && patterns::is_card_number(n) => {
                if !meter.grow(heap::text(CARD.len())) {
                    return false;
                }
                *value = CARD.into();
                true
            }
            _ => true,
        }
    }

    fn object(&self, map: &mut Map<String, Value>, scope: Scope, meter: &mut Meter<'_>) -> bool {
        if self.keys
            && let Some(Value::Object(request)) = map.get_mut("request")
            && let Some(Value::String(data)) = request.get_mut("data")
            && !meter::set(data, REQUEST_BODY, meter)
        {
            return false;
        }
        // A name/value pair, as in a list of HTTP headers.
        let named = ["key", "name", "Name", "Key"]
            .iter()
            .filter_map(|name| map.get(*name)?.as_str())
            .find_map(|name| self.class(name, false));
        for (key, value) in map.iter_mut() {
            let marker = self
                .class(key, scope.identity)
                .or_else(|| matches!(key.as_str(), "value" | "Value").then_some(named)?);
            let scrubbed = match (marker, value) {
                (Some(marker), value) => self.replace(value, marker, meter),
                (None, Value::String(text)) if scope.structural(key) => {
                    patterns::field(self, text, meter)
                }
                (None, value) => self.walk(value, scope.enter(key), meter),
            };
            if !scrubbed {
                return false;
            }
        }
        self.rename_keys(map, meter)
    }

    /// Renames keys that hold a value the patterns remove, to its marker.
    /// Keys that collide get a counter: `[Filtered: email] 2`.
    fn rename_keys(&self, map: &mut Map<String, Value>, meter: &mut Meter<'_>) -> bool {
        let (count, copies) = map
            .keys()
            .filter(|key| patterns::key_marker(self, key).is_some())
            .fold((0, 0), |(n, bytes), key| {
                (n + 1, bytes + heap::text(key.len()))
            });
        if count == 0 {
            return true;
        }
        // The list of renames, and the copies of the keys in it.
        let held = heap::text(count * size_of::<(String, &str)>()) + copies;
        if !meter.grow(held) {
            return false;
        }
        let renames: Vec<(String, &str)> = map
            .keys()
            .filter_map(|key| Some((key.clone(), patterns::key_marker(self, key)?)))
            .collect();
        // The last counter each marker took, so a thousand renames do not
        // each count up from one.
        let mut counters = [0; 4];
        let done = renames.iter().all(|(old, marker)| {
            // The new key with room for a counter, and the entry's share of
            // nodes, which renames leave less full than a parsed map's.
            if !meter.grow(heap::text(marker.len() + 24) + heap::ENTRY) {
                return false;
            }
            if let Some(value) = map.remove(old) {
                let n = &mut counters[patterns::KEY_MARKERS
                    .iter()
                    .position(|m| m == marker)
                    .unwrap_or_default()];
                map.insert(free_key(map, marker, n), value);
            }
            meter.shrink(heap::text(old.len()));
            true
        });
        meter.shrink(held);
        done
    }

    /// Replaces every string in `value` with `marker`. False when the meter
    /// refused to pay.
    fn replace(&self, value: &mut Value, marker: &'static str, meter: &mut Meter<'_>) -> bool {
        match value {
            Value::String(text) => {
                text == marker || text == CARD || meter::set(text, marker, meter)
            }
            Value::Object(map) => {
                map.values_mut()
                    .all(|value| self.replace(value, marker, meter))
                    && self.rename_keys(map, meter)
            }
            Value::Array(values) => values
                .iter_mut()
                .all(|value| self.replace(value, marker, meter)),
            _ => self.walk(value, Scope::default(), meter),
        }
    }
}

/// A key for `marker` that `map` does not hold, counting on from `n`.
fn free_key(map: &Map<String, Value>, marker: &str, n: &mut usize) -> String {
    let mut key = String::with_capacity(marker.len() + 24);
    key.push_str(marker);
    loop {
        if *n > 0 {
            key.truncate(marker.len());
            key.push(' ');
            push_number(&mut key, *n + 1);
        }
        *n += 1;
        if !map.contains_key(&key) {
            return key;
        }
    }
}

/// Appends `n` in decimal without allocating.
fn push_number(text: &mut String, mut n: usize) {
    let mut digits = [b'0'; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + u8::try_from(n % 10).unwrap_or_default();
        n /= 10;
        if n == 0 {
            break;
        }
    }
    text.extend(digits[start..].iter().map(|&d| char::from(d)));
}

#[cfg(test)]
#[path = "bypass_tests.rs"]
mod bypass_tests;
#[cfg(test)]
#[path = "key_tests.rs"]
mod key_tests;
#[cfg(test)]
#[path = "policy_tests.rs"]
mod policy_tests;
#[cfg(test)]
#[path = "structural_tests.rs"]
mod structural_tests;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
