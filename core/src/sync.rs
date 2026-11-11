//! Todos, notes, goals and calendar notes. These documents are the shell's own files, so they carry no edit times
//! or deletion marks: what changed is found by comparing each side to the last copy both agreed on.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub const KINDS: [&str; 4] = ["todo", "goals", "notes", "calendar"];
/// Well under the protocol's limit on a message, which also holds the envelope.
pub const MAX_DOC_LEN: usize = 900 * 1024;

pub fn file_name(kind: &str) -> Option<&'static str> {
    match kind {
        "todo" => Some("todo.json"),
        "goals" => Some("goals.json"),
        "notes" => Some("notes.json"),
        "calendar" => Some("calendar.json"),
        _ => None,
    }
}

pub fn empty(kind: &str) -> Value {
    if kind == "todo" {
        json!({ "tasks": [], "lists": [{ "id": 1, "name": "Daily" }], "trash": [], "dailyDate": "" })
    } else {
        json!([])
    }
}

/// `serde_json` keeps object keys sorted, so equal documents have equal revisions.
pub fn rev(doc: &Value) -> String {
    hex::encode(Sha256::digest(doc.to_string()))
}

fn id_of(item: &Value) -> Option<i64> {
    let id = item.get("id")?;
    id.as_i64().or_else(|| id.as_f64().map(|id| id as i64))
}

fn has_items(doc: &Value, text_fields: &[&str]) -> bool {
    doc.as_array().is_some_and(|items| {
        items.iter().all(|item| id_of(item).is_some() && text_fields.iter().all(|field| item.get(*field).is_some_and(Value::is_string)))
    })
}

/// What the shell can load, so that nothing else ever replaces its file.
pub fn check(kind: &str, doc: &Value) -> Result<(), String> {
    let valid = match kind {
        "todo" => {
            has_items(&doc["tasks"], &["text"])
                && has_items(&doc["lists"], &["name"])
                && has_items(&doc["trash"], &["text"])
                && doc["dailyDate"].is_string()
        }
        "goals" => {
            has_items(doc, &["title"])
                && doc.as_array().is_some_and(|goals| goals.iter().all(|goal| has_items(&goal["subgoals"], &["title"])))
        }
        "notes" => has_items(doc, &["title", "body"]),
        "calendar" => has_items(doc, &["date", "text"]),
        _ => return Err("Not something that syncs".to_string()),
    };
    if !valid {
        return Err(format!("That isn't a valid {kind} document"));
    }
    if doc.to_string().len() > MAX_DOC_LEN {
        return Err(format!("{kind} has grown too large to sync"));
    }
    Ok(())
}

/// Keeps both sides' changes. Where they changed the same field, the PC's copy wins, except that a
/// note is never lost: its older version stays as a copy.
pub fn merge(kind: &str, base: &Value, local: &Value, remote: &Value) -> Value {
    match kind {
        "notes" => merge_notes(base, local, remote),
        "todo" => merge_todo(base, local, remote),
        _ => merge_value(Some(base), local, remote),
    }
}

fn merge_value(base: Option<&Value>, local: &Value, remote: &Value) -> Value {
    if local == remote || base == Some(remote) {
        return local.clone();
    }
    if base == Some(local) {
        return remote.clone();
    }
    match (local, remote) {
        (Value::Object(local), Value::Object(remote)) => {
            Value::Object(merge_object(base.and_then(Value::as_object), local, remote))
        }
        (Value::Array(local), Value::Array(remote)) if all_have_ids(local) && all_have_ids(remote) => {
            let none = Vec::new();
            let base = base.and_then(Value::as_array).filter(|base| all_have_ids(base)).unwrap_or(&none);
            Value::Array(merge_items(base, local, remote))
        }
        _ => remote.clone(),
    }
}

fn merge_object(base: Option<&Map<String, Value>>, local: &Map<String, Value>, remote: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = Map::new();
    for key in local.keys().chain(remote.keys()) {
        if merged.contains_key(key) {
            continue;
        }
        let was = base.and_then(|base| base.get(key));
        let value = match (local.get(key), remote.get(key)) {
            (Some(local), Some(remote)) => Some(merge_value(was, local, remote)),
            (Some(only), None) | (None, Some(only)) => (was != Some(only)).then(|| only.clone()),
            (None, None) => None,
        };
        if let Some(value) = value {
            merged.insert(key.clone(), value);
        }
    }
    merged
}

fn all_have_ids(items: &[Value]) -> bool {
    items.iter().all(|item| id_of(item).is_some())
}

fn by_id(items: &[Value]) -> HashMap<i64, &Value> {
    items.iter().filter_map(|item| id_of(item).map(|id| (id, item))).collect()
}

fn ids(items: &[Value]) -> Vec<i64> {
    items.iter().filter_map(id_of).collect()
}

/// Whether the items both lists hold are in a different order.
fn reordered(base: &[i64], side: &[i64]) -> bool {
    let in_base: HashSet<_> = base.iter().collect();
    let in_side: HashSet<_> = side.iter().collect();
    let was = base.iter().filter(|id| in_side.contains(id));
    let now = side.iter().filter(|id| in_base.contains(id));
    !was.eq(now)
}

fn merge_items(base: &[Value], local: &[Value], remote: &[Value]) -> Vec<Value> {
    let (was, here, there) = (by_id(base), by_id(local), by_id(remote));
    let base_ids = ids(base);

    // Order comes from the side that changed it, the PC's when both or neither did
    let (first, second) = if reordered(&base_ids, &ids(local)) && !reordered(&base_ids, &ids(remote)) {
        (ids(local), ids(remote))
    } else {
        (ids(remote), ids(local))
    };

    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for id in first.into_iter().chain(second) {
        if !seen.insert(id) {
            continue;
        }
        let before = was.get(&id).copied();
        let item = match (here.get(&id).copied(), there.get(&id).copied()) {
            (Some(local), Some(remote)) => Some(merge_value(before, local, remote)),
            // Gone from the other side: deleted there unless this side changed it since, or is new
            (Some(only), None) | (None, Some(only)) => (before != Some(only)).then(|| only.clone()),
            (None, None) => None,
        };
        if let Some(item) = item {
            merged.push(item);
        }
    }
    merged
}

/// A task edited on one side and deleted on the other stays, so it can't also be in the trash.
fn merge_todo(base: &Value, local: &Value, remote: &Value) -> Value {
    let mut merged = merge_value(Some(base), local, remote);
    let live: HashSet<i64> = merged["tasks"].as_array().map(|tasks| ids(tasks).into_iter().collect()).unwrap_or_default();
    if let Some(trash) = merged.get_mut("trash").and_then(Value::as_array_mut) {
        trash.retain(|task| id_of(task).is_none_or(|id| !live.contains(&id)));
    }
    merged
}

fn text_clash(base: Option<&Value>, local: &Value, remote: &Value) -> bool {
    ["title", "body"].iter().any(|field| {
        let (here, there) = (&local[*field], &remote[*field]);
        here != there && base.is_none_or(|base| base[*field] != *here && base[*field] != *there)
    })
}

fn updated(note: &Value) -> i64 {
    note["updated"].as_i64().unwrap_or(0)
}

/// Newest first, as the shell keeps them.
fn merge_notes(base: &Value, local: &Value, remote: &Value) -> Value {
    let mut merged = merge_value(Some(base), local, remote);
    let (Some(local), Some(remote)) = (local.as_array(), remote.as_array()) else { return merged };
    let Some(notes) = merged.as_array_mut() else { return merged };

    let none = Vec::new();
    let was = by_id(base.as_array().unwrap_or(&none));
    let there = by_id(remote);
    let mut next_id = notes.iter().chain(local).chain(remote).filter_map(id_of).max().unwrap_or(0);
    let mut copies = Vec::new();

    for here in local {
        let Some(id) = id_of(here) else { continue };
        let Some(remote) = there.get(&id).copied() else { continue };
        if !text_clash(was.get(&id).copied(), here, remote) {
            continue;
        }
        let (winner, loser) = if updated(here) > updated(remote) { (here, remote) } else { (remote, here) };
        if let Some(note) = notes.iter_mut().find(|note| id_of(note) == Some(id)) {
            *note = winner.clone();
        }
        next_id += 1;
        let mut copy = loser.clone();
        copy["id"] = json!(next_id);
        let title = copy["title"].as_str().filter(|title| !title.is_empty()).unwrap_or("Note").to_string();
        copy["title"] = json!(format!("{title} (conflict)"));
        copies.push(copy);
    }

    notes.append(&mut copies);
    notes.sort_by_key(|note| std::cmp::Reverse(updated(note)));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: i64, text: &str) -> Value {
        json!({ "id": id, "text": text, "description": "", "done": false, "important": false, "list": -1 })
    }

    fn todo(tasks: Vec<Value>, trash: Vec<Value>) -> Value {
        json!({ "tasks": tasks, "lists": [{ "id": 1, "name": "Daily" }], "trash": trash, "dailyDate": "2026-11-4" })
    }

    fn note(id: i64, title: &str, body: &str, updated: i64) -> Value {
        json!({ "id": id, "title": title, "body": body, "created": 1, "updated": updated })
    }

    #[test]
    fn nothing_changed_stays_the_same() {
        let doc = todo(vec![task(1, "a")], vec![]);
        assert_eq!(merge("todo", &doc, &doc, &doc), doc);
    }

    #[test]
    fn a_change_on_one_side_comes_through() {
        let base = json!([{ "id": 1, "title": "a", "done": false, "created": 0, "subgoals": [] }]);
        let changed = json!([{ "id": 1, "title": "b", "done": false, "created": 0, "subgoals": [] }]);
        assert_eq!(merge("goals", &base, &changed, &base), changed);
        assert_eq!(merge("goals", &base, &base, &changed), changed);
    }

    #[test]
    fn different_fields_of_one_item_both_survive() {
        let base = todo(vec![task(1, "a")], vec![]);
        let mut local = base.clone();
        local["tasks"][0]["done"] = json!(true);
        let mut remote = base.clone();
        remote["tasks"][0]["description"] = json!("details");
        let merged = merge("todo", &base, &local, &remote);
        assert_eq!(merged["tasks"][0]["done"], json!(true));
        assert_eq!(merged["tasks"][0]["description"], json!("details"));
    }

    #[test]
    fn the_same_field_changed_on_both_goes_to_the_pc() {
        let base = todo(vec![task(1, "a")], vec![]);
        let local = todo(vec![task(1, "phone")], vec![]);
        let remote = todo(vec![task(1, "pc")], vec![]);
        assert_eq!(merge("todo", &base, &local, &remote)["tasks"][0]["text"], json!("pc"));
    }

    #[test]
    fn additions_from_both_sides_are_kept() {
        let base = todo(vec![task(1, "a")], vec![]);
        let local = todo(vec![task(1, "a"), task(2, "phone")], vec![]);
        let remote = todo(vec![task(1, "a"), task(3, "pc")], vec![]);
        let merged = merge("todo", &base, &local, &remote);
        assert_eq!(ids(merged["tasks"].as_array().unwrap()), vec![1, 3, 2]);
    }

    #[test]
    fn a_deletion_on_either_side_sticks() {
        let base = json!([note(1, "a", "x", 5), note(2, "b", "y", 4)]);
        let without_one = json!([note(2, "b", "y", 4)]);
        assert_eq!(merge("notes", &base, &without_one, &base), without_one);
        assert_eq!(merge("notes", &base, &base, &without_one), without_one);
    }

    #[test]
    fn a_deleted_item_edited_on_the_other_side_comes_back() {
        let base = json!([{ "id": 1, "title": "a", "done": false, "created": 0, "subgoals": [] }]);
        let edited = json!([{ "id": 1, "title": "kept", "done": false, "created": 0, "subgoals": [] }]);
        assert_eq!(merge("goals", &base, &edited, &json!([])), edited);
        assert_eq!(merge("goals", &base, &json!([]), &edited), edited);
    }

    #[test]
    fn a_task_trashed_on_one_side_and_edited_on_the_other_is_only_live() {
        let base = todo(vec![task(1, "a")], vec![]);
        let mut trashed = task(1, "a");
        trashed["deletedAt"] = json!(9);
        let local = todo(vec![], vec![trashed]);
        let remote = todo(vec![task(1, "edited")], vec![]);
        let merged = merge("todo", &base, &local, &remote);
        assert_eq!(merged["tasks"], json!([task(1, "edited")]));
        assert_eq!(merged["trash"], json!([]));
    }

    #[test]
    fn a_task_trashed_with_no_other_change_moves_to_the_trash() {
        let base = todo(vec![task(1, "a"), task(2, "b")], vec![]);
        let mut trashed = task(1, "a");
        trashed["deletedAt"] = json!(9);
        let local = todo(vec![task(2, "b")], vec![trashed.clone()]);
        let merged = merge("todo", &base, &local, &base);
        assert_eq!(merged["tasks"], json!([task(2, "b")]));
        assert_eq!(merged["trash"], json!([trashed]));
    }

    #[test]
    fn order_comes_from_the_side_that_changed_it() {
        let base = todo(vec![task(1, "a"), task(2, "b"), task(3, "c")], vec![]);
        let reordered = todo(vec![task(3, "c"), task(1, "a"), task(2, "b")], vec![]);
        let mut edited = base.clone();
        edited["tasks"][1]["done"] = json!(true);

        let merged = merge("todo", &base, &reordered, &edited);
        assert_eq!(ids(merged["tasks"].as_array().unwrap()), vec![3, 1, 2]);
        assert_eq!(merged["tasks"][2]["done"], json!(true));

        let merged = merge("todo", &base, &edited, &reordered);
        assert_eq!(ids(merged["tasks"].as_array().unwrap()), vec![3, 1, 2]);
    }

    #[test]
    fn subgoals_merge_like_goals() {
        let goal = |subgoals: Value| json!([{ "id": 1, "title": "g", "done": false, "created": 0, "subgoals": subgoals }]);
        let base = goal(json!([{ "id": 2, "title": "s", "done": false }]));
        let local = goal(json!([{ "id": 2, "title": "s", "done": true }]));
        let remote = goal(json!([{ "id": 2, "title": "s", "done": false }, { "id": 3, "title": "new", "done": false }]));
        let merged = merge("goals", &base, &local, &remote);
        assert_eq!(merged[0]["subgoals"][0]["done"], json!(true));
        assert_eq!(merged[0]["subgoals"][1]["title"], json!("new"));
    }

    #[test]
    fn a_note_rewritten_on_both_sides_keeps_both_versions() {
        let base = json!([note(1, "t", "old", 5)]);
        let local = json!([note(1, "t", "from the phone", 20)]);
        let remote = json!([note(1, "t", "from the pc", 10)]);
        let merged = merge("notes", &base, &local, &remote);
        let notes = merged.as_array().unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0]["id"], json!(1));
        assert_eq!(notes[0]["body"], json!("from the phone"), "the newest keeps its place");
        assert_eq!(notes[1]["body"], json!("from the pc"));
        assert_eq!(notes[1]["title"], json!("t (conflict)"));
        assert_ne!(notes[1]["id"], json!(1));

        // Once both have it, there's nothing more to do
        assert_eq!(merge("notes", &merged, &merged, &merged), merged);
    }

    #[test]
    fn notes_stay_newest_first() {
        let base = json!([note(1, "a", "x", 5), note(2, "b", "y", 4)]);
        let local = json!([note(2, "b", "changed", 9), note(1, "a", "x", 5)]);
        let remote = json!([note(3, "c", "new", 7), note(1, "a", "x", 5), note(2, "b", "y", 4)]);
        let merged = merge("notes", &base, &local, &remote);
        assert_eq!(ids(merged.as_array().unwrap()), vec![2, 3, 1]);
    }

    #[test]
    fn a_note_made_private_keeps_a_field_the_other_side_never_had() {
        let base = json!([note(1, "a", "x", 5)]);
        let mut private = note(1, "a", "x", 5);
        private["private"] = json!(true);
        assert_eq!(merge("notes", &base, &json!([private.clone()]), &base), json!([private]));
    }

    #[test]
    fn merging_the_real_shapes_with_unknown_fields_keeps_them() {
        let mut task_with_extra = task(1, "a");
        task_with_extra["future"] = json!({ "x": 1 });
        let base = todo(vec![task_with_extra.clone()], vec![]);
        let mut local = base.clone();
        local["tasks"][0]["done"] = json!(true);
        assert_eq!(merge("todo", &base, &local, &base)["tasks"][0]["future"], json!({ "x": 1 }));
    }

    #[test]
    fn a_calendar_note_keeps_a_time_and_a_repeat_set_on_different_sides() {
        let base = json!([{ "id": 1, "date": "2026-11-11", "text": "Dentist", "yearly": false }]);
        let phone = json!([{ "id": 1, "date": "2026-11-11", "text": "Dentist", "yearly": false, "time": "14:30" }]);
        let pc = json!([{ "id": 1, "date": "2026-11-11", "text": "Dentist", "yearly": false, "repeat": "weekly" }]);
        let merged = merge("calendar", &base, &phone, &pc);
        assert_eq!(merged[0]["time"], json!("14:30"));
        assert_eq!(merged[0]["repeat"], json!("weekly"));
        assert_eq!(check("calendar", &merged), Ok(()));
    }

    #[test]
    fn revisions_follow_the_content() {
        let a = json!({ "b": 1, "a": 2 });
        let b: Value = serde_json::from_str(r#"{"a":2,"b":1}"#).unwrap();
        assert_eq!(rev(&a), rev(&b));
        assert_ne!(rev(&a), rev(&json!({ "a": 2 })));
    }

    #[test]
    fn only_what_the_shell_can_load_passes() {
        for kind in KINDS {
            assert_eq!(check(kind, &empty(kind)), Ok(()), "{kind}");
        }
        assert!(check("todo", &json!([])).is_err());
        assert!(check("todo", &json!({ "tasks": [{ "text": "no id" }], "lists": [], "trash": [], "dailyDate": "" })).is_err());
        assert!(check("goals", &json!([{ "id": 1, "title": "no subgoals" }])).is_err());
        assert!(check("notes", &json!([{ "id": 1, "title": "no body" }])).is_err());
        assert!(check("calendar", &json!([{ "id": 1, "text": "no date" }])).is_err());
        assert_eq!(check("calendar", &json!([{ "id": 1, "date": "2026-11-11", "text": "a", "yearly": true }])), Ok(()));
        assert!(check("passwords", &json!([])).is_err());
    }

    #[test]
    fn a_document_too_big_to_send_is_refused() {
        let huge = "x".repeat(MAX_DOC_LEN);
        assert!(check("notes", &json!([note(1, "big", &huge, 1)])).is_err());
    }
}
