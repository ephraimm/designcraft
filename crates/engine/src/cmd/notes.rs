//! Type › Insert Footnote, Document Footnote Options, footnote inspection and editing.

use designcraft_doc::{CellAddr, FootnoteOptions, ParaFormat, Story, StoryId, TABLE_ANCHOR, Table, TextSel};
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_doc};
use crate::{Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "footnote.insert",
            "Insert Footnote",
            ["Type"],
            None,
            "{text?, series?, runIn?: bool} → {story, id, table?, row?, col?} (table, row and col are returned for a cell note) — a footnote reference at the insertion point; the caret moves into the new footnote's text (in a table cell it stays in the cell, after its reference)",
            in_story_text,
            insert
        ),
        cmd!(
            "footnote.options",
            "Document Footnote Options…",
            ["Type"],
            None,
            "{placement?: {kind: columnFoot|margin|stream, side?: inside|outside|left|right (default right), gap: pt, width: pt (gap and width required when first selecting margin)}, style?: arabic|upperRoman|lowerRoman|upperLetters|lowerLetters|symbols|kanji|hebrew|hebrewPlain, startAt?, restart?: never|page|spread|section, prefix?, suffix?, affixIn?: none|reference|text|both, bodyHidden?: bool, refHidden?: bool (zero-width unprinted anchor), refPosition?: superscript|subscript|normal|otSuperscript, refCharStyle?, paraStyle?, separator?, spaceBefore?, spaceBetween?, firstBaseline?: ascent|capHeight|leading|xHeight|emboxHeight|fixed, firstBaselineMin?, runIn?: bool (default false), runInSeparator?: string (at most 64 characters), spanColumns?, columns?: 1–4, columnGutter?: 0–144 pt, allowSplit? (default true), rule?: {on, weight, color, tint, width, offset, leftIndent}} → the options (no params: just read them)",
            has_doc,
            options
        ),
        cmd!(
            "footnote.series.create",
            "Create Note Series",
            [],
            None,
            "{name, catchwordContinuation?: string|null (at most 256 characters, no controls; null clears), placement?: stream|{kind: columnFoot|margin|stream, …}, refHidden?: bool, bodyHidden?: bool, runIn?: bool, runInSeparator?: string (at most 64 characters), …options} → {id}",
            has_doc,
            series_create
        ),
        cmd!(
            "footnote.series.options",
            "Note Series Options",
            [],
            None,
            "{series, catchwordContinuation?: string|null (at most 256 characters, no controls; null clears), runIn?: bool, runInSeparator?: string (at most 64 characters), …options} → options; placement?: {kind: columnFoot|margin|stream, side?: inside|outside|left|right (default right), gap: pt, width: pt (gap and width required when first selecting margin)}",
            has_doc,
            series_options
        ),
        cmd!(
            "footnote.series.delete",
            "Delete Note Series",
            [],
            None,
            "{series} — moves its notes to series 0; series 0 cannot be deleted",
            has_doc,
            series_delete
        ),
        cmd!(query "footnote.series.list", "Note Series", [], None, "{} → [{id, name, …options}] including series 0", has_doc, series_list),
        cmd!(
            query "footnote.list",
            "Footnotes",
            [],
            None,
            "{story?, series?} → [{story, id, series, runIn, index, anchor, text, table?, row?, col?}] in reading order within each story; a table cell's note also names its table and the cell's row and column, and its anchor is in the cell's text",
            has_doc,
            list
        ),
        cmd!(
            "footnote.setText",
            "Set Footnote Text",
            [],
            None,
            "{story, id, text, table?, row?, col?} — replaces the footnote's text (a cell's note names its table and cell)",
            has_doc,
            set_text
        ),
        cmd!(
            "footnote.delete",
            "Delete Footnote",
            [],
            None,
            "{story, id, table?, row?, col?} — deletes the reference (and so the footnote); a cell's note names its table and cell",
            has_doc,
            delete
        ),
        cmd!(
            "footnote.setRunIn",
            "Set Footnote Run In",
            [],
            None,
            "{story, ids: [note id], runIn: bool|null} — null follows the series",
            has_doc,
            set_run_in
        ),
        cmd!(
            noundo "footnote.goToReference",
            "Go to Footnote Reference",
            ["Type"],
            None,
            "{} — from footnote text, puts the caret after the footnote's reference",
            in_note,
            go_to_reference
        ),
    ]
}

fn in_story_text(s: &Session) -> std::result::Result<(), String> {
    super::has_text(s)?;
    // A table cell takes a footnote, as InDesign does; footnote text does not (no nested notes).
    match s.active().and_then(|d| d.selection.text) {
        Some(t) if t.cell.is_some_and(|c| c.footnote_id().is_some()) => Err("footnotes can't be inserted in other footnotes".into()),
        _ => Ok(()),
    }
}

fn in_note(s: &Session) -> std::result::Result<(), String> {
    super::has_text(s)?;
    match s.active().and_then(|d| d.selection.text) {
        Some(t) if t.cell.is_some_and(|c| c.footnote_id().is_some()) => Ok(()),
        _ => Err("the insertion point is not in footnote text".into()),
    }
}

fn insert(s: &mut Session, p: &Value) -> Result<Value> {
    let run_in = read_run_in(p, "footnote.insert")?;
    let text = super::text_param(p, "text");
    let series = series_id(p, "footnote.insert", true)?;
    require_series(&s.doc()?.doc, series, "footnote.insert")?;
    let r = s.edit(|d, sel| {
        let t = sel.text.ok_or_else(|| bad("footnote.insert", "no insertion point"))?;
        if t.cell.is_some() && series != 0 {
            return Err(bad("footnote.insert", "table-cell notes use series 0"));
        }
        let para = ParaFormat { style: d.note_options(series).para_style.clone(), ..Default::default() };
        let (id, r) = {
            let st = d.text_story_mut(t.story, t.cell).ok_or(designcraft_doc::DocError::NoStory(t.story))?;
            let r = t.range();
            let r = r.start.min(st.len())..r.end.min(st.len());
            st.delete(r.clone());
            let id =
                st.insert_note(r.start, &text, para).ok_or_else(|| bad("footnote.insert", "the story's footnotes don't match their references"))?;
            if let Some(note) = st.note_mut(id) {
                note.series = series;
                note.run_in = run_in;
            }
            (id, r)
        };
        let step = designcraft_doc::FOOTNOTE_REF.len_utf8();
        if t.cell.is_none() {
            d.adjust_text_ranges(t.story, r.clone(), step);
        }
        sel.text = Some(match t.cell {
            None => TextSel { story: t.story, anchor: text.len(), focus: text.len(), frame: t.frame, cell: Some(CellAddr::footnote(id)) },
            // A table cell's footnote text has no address of its own yet: the caret stays in the
            // cell, after the reference (the note's text comes from `text`).
            Some(c) => {
                sel.cells = None;
                let at = r.start + step;
                TextSel { story: t.story, anchor: at, focus: at, frame: t.frame, cell: Some(c) }
            }
        });
        Ok(match t.cell {
            Some(c) => json!({"story": t.story.0, "id": id, "table": c.table, "row": c.row, "col": c.col}),
            None => json!({"story": t.story.0, "id": id}),
        })
    })?;
    s.typing_format = None;
    Ok(r)
}

/// Overlay `p` onto `base` (objects merge key by key).
fn merge(base: &mut Value, p: &Value) {
    match (base, p) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, v) => *b = v.clone(),
    }
}

fn options(s: &mut Session, p: &Value) -> Result<Value> {
    let cur = s.doc()?.doc.footnote_options.clone();
    if p.as_object().is_none_or(|o| o.is_empty()) {
        return Ok(serde_json::to_value(&cur).unwrap_or_default());
    }
    let mut v = serde_json::to_value(&cur).unwrap_or_default();
    merge(&mut v, p);
    let next = read_options(v, "footnote.options")?;
    validate_options(&next, "footnote.options")?;
    validate_inset_placement(&s.doc()?.doc, 0, &next, "footnote.options")?;
    let out = serde_json::to_value(&next).unwrap_or_default();
    if next != cur {
        s.edit(|d, _| {
            d.footnote_options = next;
            Ok(Value::Null)
        })?;
    }
    Ok(out)
}

fn series_id(p: &Value, cmd: &str, default: bool) -> Result<u64> {
    match p.get("series") {
        None if default => Ok(0),
        Some(v) => v.as_u64().ok_or_else(|| bad(cmd, "series must be an unsigned integer")),
        _ => Err(bad(cmd, "missing series")),
    }
}

fn require_series(d: &designcraft_doc::Document, id: u64, cmd: &str) -> Result<()> {
    if id == 0 || d.note_series.iter().any(|s| s.id == id) { Ok(()) } else { Err(bad(cmd, format!("no note series {id}"))) }
}

fn validate_options(o: &FootnoteOptions, cmd: &str) -> Result<()> {
    o.check().map_err(|e| bad(cmd, e))
}

fn validate_inset_placement(doc: &designcraft_doc::Document, id: u64, options: &FootnoteOptions, cmd: &str) -> Result<()> {
    if options.placement != doc.note_options(id).placement
        && !matches!(options.placement, designcraft_doc::NotePlacement::Stream)
        && doc.placed_items().iter().any(|item| {
            item.text_frame().and_then(|f| f.options.inset_columns.as_ref()).is_some_and(|c| c.streams.iter().any(|s| s.series == Some(id)))
        })
    {
        return Err(bad(cmd, "clear this series from inset frames before changing its placement"));
    }
    Ok(())
}

#[cfg(test)]
mod inset_placement_tests {
    use super::*;

    #[test]
    fn series_continuation_validates_roundtrips_and_clears() {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        let id = s.execute("footnote.series.create", &json!({"name":"Sources", "catchwordContinuation":"בשלמא"})).unwrap()["id"].clone();
        let native = serde_json::to_vec(&s.doc().unwrap().doc).unwrap();
        let restored: designcraft_doc::Document = serde_json::from_slice(&native).unwrap();
        assert_eq!(restored.note_options(id.as_u64().unwrap()).catchword_continuation.as_deref(), Some("בשלמא"));
        for text in ["x".repeat(257), "bad\nword".into()] {
            assert!(s.execute("footnote.series.options", &json!({"series":id,"catchwordContinuation":text})).is_err());
        }
        s.execute("footnote.series.options", &json!({"series":id,"catchwordContinuation":null})).unwrap();
        assert!(s.doc().unwrap().doc.note_options(id.as_u64().unwrap()).catchword_continuation.is_none());
    }

    #[test]
    fn legacy_inset_selector_does_not_block_unchanged_placement() {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        let frame = s.execute("frame.create", &json!({"rect": [36,36,500,700], "content": "text", "text": "Body"})).unwrap();
        // Legacy series 0 inherits destination options when an inset frame is copied.
        s.edit(|doc, _| {
            doc.item_mut(designcraft_doc::ItemId(frame["id"].as_u64().unwrap())).unwrap().text_frame_mut().unwrap().options.inset_columns =
                Some(serde_json::from_value(json!({"streams": [{}, {"series": 0}]})).unwrap());
            Ok(Value::Null)
        })
        .unwrap();
        s.execute("footnote.series.options", &json!({"series": 0})).unwrap();
        s.execute("footnote.options", &json!({"separator": " / "})).unwrap();
        s.doc().unwrap().doc.check().unwrap();
    }

    #[test]
    fn inset_selected_series_cannot_change_placement_until_cleared() {
        for default in [false, true] {
            let mut s = Session::new();
            s.execute("file.new", &json!({})).unwrap();
            let frame = s.execute("frame.create", &json!({"rect": [36,36,500,700], "content": "text", "text": "Body"})).unwrap();
            let series = if default {
                s.execute("footnote.options", &json!({"placement": "stream"})).unwrap();
                json!(0)
            } else {
                s.execute("footnote.series.create", &json!({"name": "Commentary", "placement": "stream"})).unwrap()["id"].clone()
            };
            s.execute("textFrame.options", &json!({"ids": [frame["id"]], "insetColumns": {"streams": [{}, {"series": series}]}})).unwrap();
            for placement in [json!("columnFoot"), json!({"kind": "margin", "width": 72, "gap": 8})] {
                assert!(s.execute("footnote.series.options", &json!({"series": series, "placement": placement})).is_err());
                if default {
                    assert!(s.execute("footnote.options", &json!({"placement": placement})).is_err());
                }
            }
            assert!(matches!(s.doc().unwrap().doc.note_options(series.as_u64().unwrap()).placement, designcraft_doc::NotePlacement::Stream));
            s.execute("textFrame.options", &json!({"ids": [frame["id"]], "insetColumns": null})).unwrap();
            s.execute("footnote.series.options", &json!({"series": series, "placement": "columnFoot"})).unwrap();
            s.doc().unwrap().doc.check().unwrap();
        }
    }
}

fn read_options(mut value: Value, cmd: &str) -> Result<FootnoteOptions> {
    if let Some(slot) = value.get_mut("placement")
        && let Some(kind) = slot.as_str().map(str::to_owned)
    {
        *slot = json!({"kind": kind});
    }
    // Native files preserve the pre-setting no-split behavior; new API options keep the
    // documented split-enabled default. Serialized current options already contain the flag.
    if let Some(options) = value.as_object_mut() {
        options.entry("allowSplit").or_insert(json!(true));
    }
    serde_json::from_value(value).map_err(|e| bad(cmd, e.to_string()))
}

fn series_create(s: &mut Session, p: &Value) -> Result<Value> {
    let name = p
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| bad("footnote.series.create", "missing name"))?
        .to_owned();
    // New command objects use API defaults; native files may default a missing allowSplit to false.
    let mut value = serde_json::to_value(FootnoteOptions::default()).map_err(|e| bad("footnote.series.create", e.to_string()))?;
    merge(&mut value, p);
    let options = read_options(value, "footnote.series.create")?;
    validate_options(&options, "footnote.series.create")?;
    s.edit(|d, _| {
        let id = d
            .note_series
            .iter()
            .map(|s| s.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| bad("footnote.series.create", "series ids exhausted"))?;
        d.note_series.push(designcraft_doc::NoteSeries { id, name, options });
        Ok(json!({"id": id}))
    })
}

fn series_options(s: &mut Session, p: &Value) -> Result<Value> {
    let id = series_id(p, "footnote.series.options", false)?;
    require_series(&s.doc()?.doc, id, "footnote.series.options")?;
    let cur = s.doc()?.doc.note_options(id).clone();
    let mut value = serde_json::to_value(&cur).map_err(|e| bad("footnote.series.options", e.to_string()))?;
    merge(&mut value, p);
    let next = read_options(value, "footnote.series.options")?;
    validate_options(&next, "footnote.series.options")?;
    validate_inset_placement(&s.doc()?.doc, id, &next, "footnote.series.options")?;
    let out = serde_json::to_value(&next).map_err(|e| bad("footnote.series.options", e.to_string()))?;
    if next == cur {
        return Ok(out);
    }
    s.edit(|d, _| {
        if id == 0 {
            d.footnote_options = next;
        } else if let Some(series) = d.note_series.iter_mut().find(|s| s.id == id) {
            series.options = next;
        }
        Ok(Value::Null)
    })?;
    Ok(out)
}

fn series_list(s: &mut Session, _: &Value) -> Result<Value> {
    let d = &s.doc()?.doc;
    let mut list = vec![
        serde_json::to_value(designcraft_doc::NoteSeries { id: 0, name: "Footnotes".into(), options: d.footnote_options.clone() })
            .map_err(|e| bad("footnote.series.list", e.to_string()))?,
    ];
    for series in &d.note_series {
        list.push(serde_json::to_value(series).map_err(|e| bad("footnote.series.list", e.to_string()))?);
    }
    Ok(Value::Array(list))
}

fn series_delete(s: &mut Session, p: &Value) -> Result<Value> {
    let id = series_id(p, "footnote.series.delete", false)?;
    if id == 0 {
        return Err(bad("footnote.series.delete", "series 0 cannot be deleted"));
    }
    require_series(&s.doc()?.doc, id, "footnote.series.delete")?;
    s.edit(|d, _| {
        for story in d.stories.values_mut() {
            // Embedded notes also retain document-local series ids. Clear them before a
            // later create can reuse the deleted id for an unrelated series.
            let needs_reset = {
                let mut pending = vec![story.as_ref()];
                let mut found = false;
                while let Some(text) = pending.pop() {
                    if text.notes.iter().any(|note| note.series == id) {
                        found = true;
                        break;
                    }
                    pending.extend(text.notes.iter().chain(&text.endnotes).map(|note| &note.text));
                    pending.extend(text.tables.values().flat_map(|table| table.cells.iter().map(|cell| &cell.text)));
                }
                found
            };
            if !needs_reset {
                continue;
            }
            let root = std::sync::Arc::make_mut(story);
            let revision = root.rev;
            let mut pending = vec![&mut *root];
            while let Some(text) = pending.pop() {
                let mut changed = false;
                for note in &mut text.notes {
                    if note.series == id {
                        std::sync::Arc::make_mut(note).series = 0;
                        changed = true;
                    }
                }
                if changed {
                    text.rev = text.rev.wrapping_add(1);
                }
                for note in text.notes.iter_mut().chain(&mut text.endnotes) {
                    pending.push(&mut std::sync::Arc::make_mut(note).text);
                }
                for table in text.tables.values_mut() {
                    for cell in &mut std::sync::Arc::make_mut(table).cells {
                        pending.push(&mut cell.text);
                    }
                }
            }
            drop(pending);
            root.rev = revision.wrapping_add(1);
        }
        d.update_placed_items(
            |it| {
                it.text_frame().is_some_and(|tf| {
                    tf.options.note_stream.is_some_and(|stream| stream.series == id)
                        || tf.options.inset_columns.as_ref().is_some_and(|c| c.streams.iter().any(|s| s.series == Some(id)))
                })
            },
            |it| {
                if let Some(tf) = it.text_frame_mut() {
                    if tf.options.note_stream.is_some_and(|stream| stream.series == id) {
                        tf.options.note_stream = None;
                    }
                    if let Some(inset) = &mut tf.options.inset_columns {
                        inset.streams.retain(|s| s.series != Some(id));
                    }
                }
            },
        );
        d.note_series.retain(|s| s.id != id);
        for variable in &mut d.text_variables {
            if let designcraft_doc::vars::VarKind::NextPageFirstWord { series: Some(series), .. } = &mut variable.kind
                && *series == id
            {
                *series = 0;
            }
        }
        Ok(Value::Null)
    })
}

fn story_id(p: &Value, cmd: &str) -> Result<StoryId> {
    p.get("story").and_then(Value::as_u64).map(StoryId).ok_or_else(|| bad(cmd, "missing story"))
}

fn read_run_in(p: &Value, command: &str) -> Result<Option<bool>> {
    match p.get("runIn") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(bad(command, "runIn must be a boolean or null")),
    }
}

fn set_run_in(s: &mut Session, p: &Value) -> Result<Value> {
    let command = "footnote.setRunIn";
    let sid = story_id(p, command)?;
    let value = read_run_in(p, command)?;
    let ids = p
        .get("ids")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(command, "missing ids"))?
        .iter()
        .map(|v| v.as_u64().ok_or_else(|| bad(command, "ids must be unsigned integers")))
        .collect::<Result<Vec<_>>>()?;
    let story = s.doc()?.doc.story(sid).ok_or(designcraft_doc::DocError::NoStory(sid))?;
    for id in &ids {
        if story.note(*id).is_none() {
            return Err(bad(command, format!("no footnote {id}")));
        }
    }
    s.edit(|d, _| {
        let story = d.story_mut(sid).ok_or(designcraft_doc::DocError::NoStory(sid))?;
        for id in &ids {
            if let Some(note) = story.note_mut(*id) {
                note.run_in = value;
            }
        }
        Ok(json!({"changed": ids.len()}))
    })
}

fn note_id(p: &Value, cmd: &str) -> Result<u64> {
    p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(cmd, "missing id"))
}

/// Moves a caret or selection in story `sid`'s own text (not a cell or note) back by the length of `range` when it lies
/// after `range.start`, or to `range.start` when it lies inside `range`.
pub(super) fn follow_delete(sel: &mut designcraft_doc::Selection, sid: StoryId, range: std::ops::Range<usize>) {
    follow_delete_in_text(sel, sid, None, range);
}

fn follow_delete_in_text(sel: &mut designcraft_doc::Selection, sid: StoryId, cell: Option<CellAddr>, range: std::ops::Range<usize>) {
    if let Some(t) = sel.text.as_mut().filter(|t| t.story == sid && t.cell == cell) {
        let back = |i: usize| if i > range.start { i - (i - range.start).min(range.len()) } else { i };
        t.anchor = back(t.anchor);
        t.focus = back(t.focus);
    }
}

fn list(s: &mut Session, p: &Value) -> Result<Value> {
    let d = &s.doc()?.doc;
    let only = p.get("story").and_then(Value::as_u64).map(StoryId);
    let mut out = Vec::new();
    for st in d.stories.values().filter(|st| only.is_none_or(|o| o == st.id)) {
        collect_notes(st, st.id, None, &mut out);
    }
    out.retain(|n| p.get("series").and_then(Value::as_u64).is_none_or(|id| n["series"] == id));
    Ok(Value::Array(out))
}

/// Append `st`'s notes, and those of the tables anchored in it, in reading order. `cell` is the
/// table cell `st` is (`None` for a story's own text), and `root` is the document story the notes
/// belong to, so a cell's note names the story a command must open.
fn collect_notes(st: &Story, root: StoryId, cell: Option<(u64, usize, usize)>, out: &mut Vec<Value>) {
    let mut k = 0;
    for (at, ch) in st.text.char_indices() {
        if ch == designcraft_doc::FOOTNOTE_REF {
            if let Some(n) = st.notes.get(k) {
                let mut o = serde_json::Map::new();
                o.insert("story".into(), json!(root.0));
                o.insert("id".into(), json!(n.id));
                o.insert("series".into(), json!(n.series));
                o.insert("runIn".into(), json!(n.run_in));
                o.insert("index".into(), json!(k));
                o.insert("anchor".into(), json!(at));
                o.insert("text".into(), json!(n.text.text));
                if let Some((table, row, col)) = cell {
                    o.insert("table".into(), json!(table));
                    o.insert("row".into(), json!(row));
                    o.insert("col".into(), json!(col));
                }
                out.push(Value::Object(o));
            }
            k += 1;
        } else if ch == TABLE_ANCHOR && cell.is_none() {
            // ponytail: a table nested in a cell (only imports make one) is skipped: the commands
            // resolve `table` in the story's own tables, so its notes need a full cell path first.
            let Some(t) = st.table_at(at).and_then(|id| st.tables.get(&id)) else { continue };
            for (row, col) in cell_reading_order(t) {
                if let Some(c) = t.cell(row, col) {
                    collect_notes(&c.text, root, Some((t.id, row, col)), out);
                }
            }
        }
    }
}

/// The cells a table is read in: row by row, each from its first column (on the right in a
/// right-to-left table, which lays its columns out mirrored).
fn cell_reading_order(t: &Table) -> Vec<(usize, usize)> {
    let (nr, nc) = (t.nrows(), t.ncols());
    let owners = t.owners();
    let mut cells = Vec::new();
    for r in 0..nr {
        for c in 0..nc {
            let Some(at) = r.checked_mul(nc).and_then(|x| x.checked_add(c)) else { continue };
            if owners.get(at) == Some(&(r, c)) {
                cells.push((r, c));
            }
        }
    }
    cells
}

/// The cell a footnote command names, when `table` is given (`row` and `col` then follow it).
fn note_cell(p: &Value, cmd: &str) -> Result<Option<CellAddr>> {
    let Some(table) = p.get("table").filter(|v| !v.is_null()) else {
        if ["row", "col"].iter().any(|key| p.get(key).is_some_and(|v| !v.is_null())) {
            return Err(bad(cmd, "a cell needs `table`"));
        }
        return Ok(None);
    };
    let table = table.as_u64().ok_or_else(|| bad(cmd, "`table` must be a table id"))?;
    let index = |key: &str| -> Result<usize> {
        let v = p.get(key).and_then(Value::as_u64).ok_or_else(|| bad(cmd, format!("a cell needs `{key}`")))?;
        usize::try_from(v).map_err(|_| bad(cmd, format!("`{key}` is too large")))
    };
    Ok(Some(CellAddr { table, row: index("row")?, col: index("col")? }))
}

fn note_story<'a>(d: &'a mut designcraft_doc::Document, sid: StoryId, cell: Option<CellAddr>, cmd: &str) -> Result<&'a mut Story> {
    d.text_story_mut(sid, cell).ok_or_else(|| match cell {
        Some(c) => bad(cmd, format!("no table {} cell {}:{} in story {}", c.table, c.row, c.col, sid.0)),
        None => bad(cmd, format!("no story {}", sid.0)),
    })
}

fn set_text(s: &mut Session, p: &Value) -> Result<Value> {
    let sid = story_id(p, "footnote.setText")?;
    let id = note_id(p, "footnote.setText")?;
    let cell = note_cell(p, "footnote.setText")?;
    let text = super::text_param(p, "text");
    s.edit(|d, sel| {
        let st = note_story(d, sid, cell, "footnote.setText")?;
        let n = st.note_mut(id).ok_or_else(|| bad("footnote.setText", format!("no footnote {id}")))?;
        let len = n.text.len();
        n.text.replace(0..len, &text);
        // A caret in the replaced text goes to its end.
        if let Some(t) = sel.text.as_mut().filter(|t| cell.is_none() && t.story == sid && t.cell == Some(CellAddr::footnote(id))) {
            (t.anchor, t.focus) = (n.text.len(), n.text.len());
        }
        Ok(Value::Null)
    })
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let sid = story_id(p, "footnote.delete")?;
    let id = note_id(p, "footnote.delete")?;
    let cell = note_cell(p, "footnote.delete")?;
    let moved = s.edit(|d, sel| {
        let st = note_story(d, sid, cell, "footnote.delete")?;
        let at = st.note_anchor(id).ok_or_else(|| bad("footnote.delete", format!("no footnote {id}")))?;
        let len = designcraft_doc::FOOTNOTE_REF.len_utf8();
        follow_delete_in_text(sel, sid, cell, at..at + len);
        st.delete(at..at + len);
        if cell.is_none() {
            d.adjust_text_ranges(sid, at..at + len, 0);
        }
        let moved = cell.is_none() && sel.text.is_some_and(|t| t.story == sid && t.cell == Some(CellAddr::footnote(id)));
        if moved {
            sel.text = Some(TextSel::caret(sid, at));
        }
        Ok(moved)
    })?;
    if moved {
        s.typing_format = None;
    }
    Ok(Value::Null)
}

fn go_to_reference(s: &mut Session, _: &Value) -> Result<Value> {
    let st = s.doc_mut()?;
    let t = st.selection.text.ok_or_else(|| bad("footnote.goToReference", "no caret"))?;
    let id = t.cell.and_then(|c| c.footnote_id()).ok_or_else(|| bad("footnote.goToReference", "not in footnote text"))?;
    let at = st.doc.story(t.story).and_then(|x| x.note_anchor(id)).ok_or_else(|| bad("footnote.goToReference", "footnote not found"))?;
    let pos = at + designcraft_doc::FOOTNOTE_REF.len_utf8();
    st.selection.text = Some(TextSel { anchor: pos, focus: pos, cell: None, ..t });
    st.bump_revision();
    s.typing_format = None;
    Ok(json!({"story": t.story.0, "pos": pos}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_text() -> (Session, StoryId) {
        let mut s = Session::new();
        s.execute("file.new", &json!({"pages": 1})).unwrap();
        let r = s.execute("frame.create", &json!({"rect": [72, 72, 300, 400], "content": "text", "text": "Body text with a note here."})).unwrap();
        let sid = s.doc().unwrap().doc.item(designcraft_doc::ItemId(r["id"].as_u64().unwrap())).unwrap().text_frame().unwrap().story;
        (s, sid)
    }

    #[test]
    fn body_hidden_series_omits_label_and_separator_and_preserves_reference() {
        let (mut s, sid) = session_with_text();
        let series = s.execute("footnote.series.create", &json!({"name": "Sources", "separator": "XYZ"})).unwrap()["id"].clone();
        s.execute("text.select", &json!({"story": sid.0, "anchor": 4, "focus": 4})).unwrap();
        s.execute("footnote.insert", &json!({"series": series, "text": "A source."})).unwrap();
        let composed = |s: &Session| {
            let d = &s.doc().unwrap().doc;
            let cs = s.cache.get(d, sid, None);
            let body: String =
                cs.frames[0].notes[0].text.frames.iter().flat_map(|f| &f.lines).flat_map(|l| &l.glyphs).map(|g| g.rendered_char).collect();
            let main: Vec<_> = cs.frames[0].lines.iter().flat_map(|l| &l.glyphs).map(|g| (g.rendered_char, g.x, g.adv)).collect();
            (body, main)
        };
        let (numbered, reference) = composed(&s);
        assert!(numbered.contains("1XYZ"), "{numbered}");
        s.execute("footnote.series.options", &json!({"series": series, "bodyHidden": true})).unwrap();
        let (hidden, after) = composed(&s);
        assert_eq!(hidden, "A source.");
        assert_eq!(reference, after);
        let bytes = designcraft_format::save(&s.doc().unwrap().doc).unwrap();
        let back = designcraft_format::load(&bytes).unwrap();
        assert_eq!(serde_json::to_value(back.note_options(series.as_u64().unwrap())).unwrap()["bodyHidden"], true);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(composed(&s).0, numbered);
        s.execute("footnote.options", &json!({"bodyHidden": true})).unwrap();
        assert_eq!(s.execute("footnote.options", &json!({})).unwrap()["bodyHidden"], true);
    }

    #[test]
    fn footnote_options_omitted_split_preserves_the_document_setting() {
        let (mut s, _) = session_with_text();
        assert_eq!(s.execute("footnote.options", &json!({"spaceBefore": 2.0})).unwrap()["allowSplit"], true);
        s.execute("footnote.options", &json!({"allowSplit": false})).unwrap();
        assert_eq!(s.execute("footnote.options", &json!({"spaceBefore": 3.0})).unwrap()["allowSplit"], false);
    }

    #[test]
    fn generated_hebrew_note_numbers_read_right_to_left() {
        let (mut s, sid) = session_with_text();
        s.execute("footnote.options", &json!({"style": "hebrewPlain", "startAt": 15, "separator": " "})).unwrap();
        s.execute("text.select", &json!({"story": sid.0, "anchor": 4, "focus": 4})).unwrap();
        s.execute("footnote.insert", &json!({"text": "Note"})).unwrap();
        let d = s.doc().unwrap().doc.clone();
        let cs = s.cache.get(&d, sid, None);
        let x = |glyphs: &[designcraft_compose::PlacedGlyph], ch: char| glyphs.iter().find(|g| g.rendered_char == ch).unwrap().x;
        let body = &cs.frames[0].notes[0].text.frames[0].lines[0].glyphs;
        assert!(x(body, 'ט') > x(body, 'ו'), "note body numeral reads left to right");
        let main = &cs.frames[0].lines.iter().find(|l| l.glyphs.iter().any(|g| g.rendered_char == 'ט')).unwrap().glyphs;
        assert!(x(main, 'ט') > x(main, 'ו'), "reference numeral reads left to right");
    }

    #[test]
    fn hebrew_note_options_parse_both_styles() {
        let (mut s, _) = session_with_text();
        for style in ["hebrew", "hebrewPlain"] {
            for command in ["footnote.options", "endnote.options"] {
                let options = s.execute(command, &json!({"style": style})).unwrap();
                assert_eq!(options["style"], style);
            }
        }
    }

    #[test]
    fn run_in_options_override_validation_and_undo() {
        let (mut s, sid) = session_with_text();
        let options = s.execute("footnote.options", &json!({"runIn":true,"runInSeparator":" · "})).unwrap();
        assert_eq!(options["runIn"], true);
        s.execute("text.select", &json!({"story":sid.0,"anchor":0,"focus":0})).unwrap();
        let id = s.execute("footnote.insert", &json!({"text":"Note","runIn":false})).unwrap()["id"].as_u64().unwrap();
        assert_eq!(s.execute("footnote.list", &json!({})).unwrap()[0]["runIn"], false);
        s.execute("footnote.setRunIn", &json!({"story":sid.0,"ids":[id],"runIn":true})).unwrap();
        assert_eq!(s.execute("footnote.list", &json!({})).unwrap()[0]["runIn"], true);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.execute("footnote.list", &json!({})).unwrap()[0]["runIn"], false);
        s.execute("footnote.setRunIn", &json!({"story":sid.0,"ids":[id],"runIn":null})).unwrap();
        assert!(s.execute("footnote.list", &json!({})).unwrap()[0]["runIn"].is_null());
        assert!(s.execute("footnote.setRunIn", &json!({"story":sid.0,"ids":[id],"runIn":"yes"})).is_err());
        assert!(s.execute("footnote.options", &json!({"runInSeparator":"א".repeat(65)})).is_err());
        assert!(s.execute("footnote.series.create", &json!({"name":"Bad","runInSeparator":"x".repeat(65)})).is_err());
        assert_eq!(s.execute("footnote.options", &json!({})).unwrap()["runInSeparator"], " · ");
    }

    #[test]
    fn insert_type_and_undo_footnotes() {
        let (mut s, sid) = session_with_text();
        s.execute("text.select", &json!({"story": sid.0, "anchor": 16, "focus": 16})).unwrap();
        let r = s.execute("footnote.insert", &json!({})).unwrap();
        let id = r["id"].as_u64().unwrap();
        assert_eq!(r, json!({"story": sid.0, "id": id}));
        // Typing goes into the footnote.
        s.execute("text.insert", &json!({"text": "A source."})).unwrap();
        let st = s.doc().unwrap().doc.story(sid).unwrap().clone();
        assert_eq!(st.notes.len(), 1);
        assert_eq!(st.notes[0].text.text, "A source.");
        assert_eq!(st.text.matches(designcraft_doc::FOOTNOTE_REF).count(), 1);
        // The footnote is laid out in the frame.
        let d = s.doc().unwrap().doc.clone();
        let cs = s.cache.get(&d, sid, None);
        assert_eq!(cs.frames[0].notes.len(), 1);
        // Back to the reference.
        let r = s.execute("footnote.goToReference", &json!({})).unwrap();
        assert_eq!(r["pos"], json!(16 + designcraft_doc::FOOTNOTE_REF.len_utf8()));
        // Listing, editing, options.
        let l = s.execute("footnote.list", &json!({})).unwrap();
        assert_eq!(l[0]["text"], "A source.");
        s.execute("footnote.setText", &json!({"story": sid.0, "id": id, "text": "Changed."})).unwrap();
        assert_eq!(s.doc().unwrap().doc.story(sid).unwrap().notes[0].text.text, "Changed.");
        let o = s.execute("footnote.options", &json!({"style": "lowerRoman", "rule": {"width": 100.0}})).unwrap();
        assert_eq!(o["style"], "lowerRoman");
        assert_eq!(o["rule"]["width"], 100.0);
        assert_eq!(o["rule"]["on"], true);
        assert!(s.execute("footnote.options", &json!({"restart": "bogus"})).is_err());
        // Deleting removes the reference; undo brings everything back.
        s.execute("footnote.delete", &json!({"story": sid.0, "id": id})).unwrap();
        assert!(s.doc().unwrap().doc.story(sid).unwrap().notes.is_empty());
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.doc().unwrap().doc.story(sid).unwrap().notes.len(), 1);
    }

    #[test]
    fn footnote_caret_moves_drop_the_pending_typing_format() {
        let (mut s, sid) = session_with_text();
        let pending = |s: &mut Session| {
            s.execute("type.char", &json!({"attrs": {"size": 30}})).unwrap();
            assert!(s.typing_format.is_some());
        };
        // Formatting chosen in footnote text, then away to the reference and back to the same caret (a new
        // footnote takes the deleted one's id): typing there is in the footnote text's own format.
        s.execute("text.select", &json!({"story": sid.0, "anchor": 16, "focus": 16})).unwrap();
        let id = s.execute("footnote.insert", &json!({"text": "A source."})).unwrap()["id"].as_u64().unwrap();
        pending(&mut s);
        s.execute("footnote.goToReference", &json!({})).unwrap();
        s.execute("footnote.delete", &json!({"story": sid.0, "id": id})).unwrap();
        let again = s.execute("footnote.insert", &json!({"text": "A source."})).unwrap()["id"].as_u64().unwrap();
        assert_eq!(again, id);
        s.execute("text.insert", &json!({"text": "x"})).unwrap();
        assert_eq!(s.doc().unwrap().doc.story(sid).unwrap().notes[0].text.char_format_at(10).over.size, None);
        // Each command that moves the caret drops it.
        pending(&mut s);
        s.execute("footnote.goToReference", &json!({})).unwrap();
        assert!(s.typing_format.is_none(), "footnote.goToReference");
        pending(&mut s);
        let id = s.execute("footnote.insert", &json!({"text": "y"})).unwrap()["id"].as_u64().unwrap();
        assert!(s.typing_format.is_none(), "footnote.insert");
        pending(&mut s);
        s.execute("footnote.delete", &json!({"story": sid.0, "id": id})).unwrap();
        assert!(s.typing_format.is_none(), "footnote.delete");
    }

    #[test]
    fn not_in_footnote_text() {
        let (mut s, sid) = session_with_text();
        s.execute("text.select", &json!({"story": sid.0, "anchor": 0, "focus": 0})).unwrap();
        s.execute("footnote.insert", &json!({"text": "x"})).unwrap();
        // Caret is now in footnote text: no nested footnotes.
        let e = s.execute("footnote.insert", &json!({})).unwrap_err().to_string();
        assert!(e.contains("other footnotes"), "{e}");
    }

    /// A table cell takes a footnote, as InDesign does: the reference and its note live in the
    /// cell's own story, the way an imported cell footnote is stored.
    #[test]
    fn insert_footnote_in_a_table_cell() {
        let (mut s, sid, tid) = session_with_table();
        let r = s.execute("footnote.insert", &json!({"text": "A cell note."})).unwrap();
        let id = r["id"].as_u64().unwrap();
        assert_eq!(r["story"], sid.0);
        assert_eq!(r["table"], tid);
        assert_eq!(r["row"], 0);
        assert_eq!(r["col"], 0);
        let st = s.doc().unwrap().doc.story(sid).unwrap().clone();
        let cell = st.tables.get(&tid).unwrap().cell(0, 0).unwrap().clone();
        assert_eq!(cell.text.text, format!("Left{}", designcraft_doc::FOOTNOTE_REF));
        assert_eq!(cell.text.notes.len(), 1);
        assert_eq!(cell.text.notes[0].id, id);
        assert_eq!(cell.text.notes[0].text.text, "A cell note.");
        // The body story takes no part.
        assert!(st.notes.is_empty());
        assert_eq!(st.text.matches(designcraft_doc::FOOTNOTE_REF).count(), 0);
        // The caret stays in the cell, after the reference; typing carries on in the cell and the
        // note keeps its text.
        let t = s.doc().unwrap().selection.text.unwrap();
        assert_eq!(t.cell, Some(CellAddr { table: tid, row: 0, col: 0 }));
        let after_ref = "Left".len() + designcraft_doc::FOOTNOTE_REF.len_utf8();
        assert_eq!((t.anchor, t.focus), (after_ref, after_ref));
        assert!(s.doc().unwrap().selection.cells.is_none(), "the cell is no longer selected as a whole");
        s.execute("text.insert", &json!({"text": "!"})).unwrap();
        let cell = s.doc().unwrap().doc.story(sid).unwrap().tables.get(&tid).unwrap().cell(0, 0).unwrap().clone();
        assert_eq!(cell.text.text, format!("Left{}!", designcraft_doc::FOOTNOTE_REF));
        assert_eq!(cell.text.notes[0].text.text, "A cell note.");
        s.doc().unwrap().doc.check().unwrap();
        // Undo takes the reference and its note away; redo brings them back.
        s.execute("edit.undo", &json!({})).unwrap(); // the typed "!"
        s.execute("edit.undo", &json!({})).unwrap(); // the footnote
        let cell = s.doc().unwrap().doc.story(sid).unwrap().tables.get(&tid).unwrap().cell(0, 0).unwrap().clone();
        assert_eq!(cell.text.text, "Left");
        assert!(cell.text.notes.is_empty());
        s.execute("edit.redo", &json!({})).unwrap();
        let cell = s.doc().unwrap().doc.story(sid).unwrap().tables.get(&tid).unwrap().cell(0, 0).unwrap().clone();
        assert_eq!(cell.text.text, format!("Left{}", designcraft_doc::FOOTNOTE_REF));
        assert_eq!(cell.text.notes.len(), 1);
        assert_eq!(cell.text.notes[0].text.text, "A cell note.");
    }

    /// A cell footnote survives Save and Open.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_cell_footnote_survives_save_and_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cell-footnote.designcraft");
        let (mut s, sid, tid) = session_with_table();
        s.execute("footnote.insert", &json!({"text": "A cell note."})).unwrap();
        s.execute("file.saveAs", &json!({"path": path.to_string_lossy()})).unwrap();
        let mut back = Session::new();
        back.execute("file.open", &json!({"path": path.to_string_lossy()})).unwrap();
        let st = back.doc().unwrap().doc.story(sid).unwrap().clone();
        let cell = st.tables.get(&tid).unwrap().cell(0, 0).unwrap().clone();
        assert_eq!(cell.text.text, format!("Left{}", designcraft_doc::FOOTNOTE_REF));
        assert_eq!(cell.text.notes[0].text.text, "A cell note.");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn inserted_cell_note_identity_lists_edits_undoes_saves_and_deletes() {
        let (mut s, sid, tid) = session_with_table();
        s.execute("text.select", &json!({"story": sid.0, "anchor": 0, "focus": 0})).unwrap();
        let body = s.execute("footnote.insert", &json!({"text": "Body note."})).unwrap();
        let body_text = s.doc().unwrap().doc.story(sid).unwrap().text.clone();
        s.execute("table.select", &json!({"story": sid.0, "table": tid, "rows": [0, 0], "cols": [0, 0]})).unwrap();
        s.execute("text.move", &json!({"dir": "storyEnd"})).unwrap();
        let identity = s.execute("footnote.insert", &json!({"text": "Cell note."})).unwrap();
        assert_eq!(identity["id"], body["id"], "note ids are local to each story");
        assert_eq!(identity, json!({"story": sid.0, "id": body["id"], "table": tid, "row": 0, "col": 0}));
        let listed = s.execute("footnote.list", &json!({})).unwrap();
        assert!(
            listed.as_array().unwrap().iter().any(|note| { ["story", "id", "table", "row", "col"].iter().all(|key| note[*key] == identity[*key]) })
        );
        let check = |s: &Session, cell_text: Option<&str>| {
            let doc = &s.doc().unwrap().doc;
            let st = doc.story(sid).unwrap();
            assert_eq!(st.text, body_text);
            assert_eq!(st.text.matches(designcraft_doc::FOOTNOTE_REF).count(), 1);
            assert_eq!(st.notes.len(), 1);
            assert_eq!(st.notes[0].id, body["id"].as_u64().unwrap());
            assert_eq!(st.notes[0].text.text, "Body note.");
            let cell = st.tables.get(&tid).unwrap().cell(0, 0).unwrap();
            if let Some(text) = cell_text {
                assert_eq!(cell.text.notes.len(), 1);
                assert_eq!(cell.text.notes[0].id, identity["id"].as_u64().unwrap());
                assert_eq!(cell.text.notes[0].text.text, text);
                assert_eq!(cell.text.text, format!("Left{}", designcraft_doc::FOOTNOTE_REF));
            } else {
                assert!(cell.text.notes.is_empty());
                assert_eq!(cell.text.text, "Left");
            }
            doc.check().unwrap();
        };
        check(&s, Some("Cell note."));
        let mut edit = identity.clone();
        edit["text"] = json!("Changed cell note.");
        s.execute("footnote.setText", &edit).unwrap();
        check(&s, Some("Changed cell note."));
        s.execute("edit.undo", &json!({})).unwrap();
        check(&s, Some("Cell note."));
        s.execute("edit.redo", &json!({})).unwrap();
        check(&s, Some("Changed cell note."));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cell-note-identity.designcraft");
        s.execute("file.saveAs", &json!({"path": path.to_string_lossy()})).unwrap();
        let mut back = Session::new();
        back.execute("file.open", &json!({"path": path.to_string_lossy()})).unwrap();
        check(&back, Some("Changed cell note."));
        back.execute("footnote.delete", &identity).unwrap();
        check(&back, None);
    }

    /// A text frame whose 2×2 table has "Left" in its first cell and a caret in it.
    fn session_with_table() -> (Session, StoryId, u64) {
        let mut s = Session::new();
        s.execute("file.new", &json!({"pages": 1})).unwrap();
        let frame = s.execute("frame.create", &json!({"rect": [36, 36, 300, 400], "content": "text"})).unwrap();
        let sid = StoryId(frame["story"].as_u64().unwrap());
        let tid = s.execute("table.insert", &json!({"rows": 2, "cols": 2})).unwrap()["table"].as_u64().unwrap();
        s.execute("table.setCell", &json!({"story": sid.0, "table": tid, "row": 0, "col": 0, "text": "Left"})).unwrap();
        s.execute("table.select", &json!({"story": sid.0, "table": tid, "rows": [0, 0], "cols": [0, 0]})).unwrap();
        s.execute("text.move", &json!({"dir": "storyEnd"})).unwrap();
        (s, sid, tid)
    }

    #[test]
    fn huge_start_at_does_not_overflow() {
        let (mut s, sid) = session_with_text();
        s.execute("text.select", &json!({"story": sid.0, "anchor": 16, "focus": 16})).unwrap();
        s.execute("footnote.insert", &json!({"text": "x"})).unwrap();
        s.execute("footnote.options", &json!({"startAt": 4294967295u32})).unwrap();
        let d = s.doc().unwrap().doc.clone();
        let cs = s.cache.get(&d, sid, None);
        assert_eq!(cs.frames[0].notes.len(), 1);
    }

    #[test]
    fn deleting_a_footnote_keeps_the_caret_on_its_character() {
        let n = designcraft_doc::FOOTNOTE_REF.len_utf8();
        // (anchor, focus) in the story text before the delete, then after it.
        for (before, after) in [((17 + n, 21 + n), (17, 21)), ((5, 21 + n), (5, 21)), ((5, 5), (5, 5)), ((16 + n, 16 + n), (16, 16))] {
            let (mut s, sid) = session_with_text();
            s.execute("text.select", &json!({"story": sid.0, "anchor": 16, "focus": 16})).unwrap();
            let id = s.execute("footnote.insert", &json!({"text": "x"})).unwrap()["id"].as_u64().unwrap();
            s.execute("text.select", &json!({"story": sid.0, "anchor": before.0, "focus": before.1})).unwrap();
            s.execute("footnote.delete", &json!({"story": sid.0, "id": id})).unwrap();
            let t = s.doc().unwrap().selection.text.unwrap();
            assert_eq!((t.anchor, t.focus), after, "selected {before:?}");
        }
    }

    #[test]
    fn replacing_footnote_text_puts_its_caret_at_the_end() {
        let (mut s, sid) = session_with_text();
        s.execute("text.select", &json!({"story": sid.0, "anchor": 16, "focus": 16})).unwrap();
        let id = s.execute("footnote.insert", &json!({"text": "abcd"})).unwrap()["id"].as_u64().unwrap();
        // The caret between "c" and "d" is inside the second "é" of the new text.
        let t = s.doc().unwrap().selection.text.unwrap();
        assert_eq!(t.cell, Some(CellAddr::footnote(id)));
        s.doc_mut().unwrap().selection.text = Some(TextSel { anchor: 3, focus: 3, ..t });
        s.execute("footnote.setText", &json!({"story": sid.0, "id": id, "text": "éé"})).unwrap();
        let t = s.doc().unwrap().selection.text.unwrap();
        assert_eq!((t.anchor, t.focus), (4, 4));
        s.execute("text.delete", &json!({})).unwrap();
        assert_eq!(s.doc().unwrap().doc.story(sid).unwrap().notes[0].text.text, "é");
    }

    /// A story with a note before a 2 × 2 table, notes in three cells (row 1 left, row 1 right,
    /// row 2 right) and a note after it — the shape of the shared `cell-footnotes.idml` repro.
    fn session_with_cell_notes() -> (Session, StoryId, u64) {
        let mut s = Session::new();
        s.execute("file.new", &json!({"pages": 1})).unwrap();
        let r = s.execute("frame.create", &json!({"rect": [72, 72, 300, 400], "content": "text", "text": "Before table. After table."})).unwrap();
        let sid = s.doc().unwrap().doc.item(designcraft_doc::ItemId(r["id"].as_u64().unwrap())).unwrap().text_frame().unwrap().story;
        let at = "Before table.".len();
        s.execute("text.select", &json!({"story": sid.0, "anchor": at, "focus": at})).unwrap();
        s.execute("footnote.insert", &json!({"text": "Note 1 (body)."})).unwrap();
        let after_ref = at + designcraft_doc::FOOTNOTE_REF.len_utf8();
        s.execute("text.select", &json!({"story": sid.0, "anchor": after_ref, "focus": after_ref})).unwrap();
        let tid = s.execute("table.insert", &json!({"rows": 2, "cols": 2})).unwrap()["table"].as_u64().unwrap();
        {
            let doc = std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc);
            for (row, col, text, note) in [
                (0, 0, "Left 1", "Note A (left cell, row 1)."),
                (0, 1, "Right 1", "Note B (right cell, row 1)."),
                (1, 1, "Right 2", "Note C (right cell, row 2)."),
            ] {
                let st = doc.text_story_mut(sid, Some(CellAddr { table: tid, row, col })).unwrap();
                st.insert(st.len(), text);
                st.insert_note(st.len(), note, ParaFormat::default());
            }
        }
        let after = s.doc().unwrap().doc.story(sid).unwrap().text.find("After table").unwrap() + "After table".len();
        s.execute("text.select", &json!({"story": sid.0, "anchor": after, "focus": after})).unwrap();
        s.execute("footnote.insert", &json!({"text": "Note 2 (body)."})).unwrap();
        (s, sid, tid)
    }

    #[test]
    fn list_includes_table_cell_notes_in_reading_order() {
        let (mut s, sid, tid) = session_with_cell_notes();
        let l = s.execute("footnote.list", &json!({})).unwrap();
        let notes = l.as_array().unwrap();
        let texts: Vec<&str> = notes.iter().filter_map(|n| n["text"].as_str()).collect();
        assert_eq!(
            texts,
            ["Note 1 (body).", "Note A (left cell, row 1).", "Note B (right cell, row 1).", "Note C (right cell, row 2).", "Note 2 (body)."]
        );
        // Body notes keep the shape they had: no cell fields.
        assert!(notes[0].get("table").is_none());
        assert!(notes[0].get("row").is_none());
        assert_eq!(notes[0]["index"], 0);
        assert_eq!(notes[4]["index"], 1);
        // A cell's note names the document story, its table and the cell to open and edit it.
        let at = |n: &Value| (n["story"].as_u64(), n["table"].as_u64(), n["row"].as_u64(), n["col"].as_u64());
        assert_eq!(at(&notes[1]), (Some(sid.0), Some(tid), Some(0), Some(0)));
        assert_eq!(at(&notes[2]), (Some(sid.0), Some(tid), Some(0), Some(1)));
        assert_eq!(at(&notes[3]), (Some(sid.0), Some(tid), Some(1), Some(1)));
        assert_eq!(notes[1]["anchor"], 6, "the reference offset within the cell's text");
    }

    #[test]
    fn list_leaves_out_notes_in_nested_tables() {
        // An imported cell can hold a nested table whose id matches the outer one; the footnote
        // commands resolve a listed cell in the story's own tables, so listing it would point there.
        let (mut s, sid, tid) = session_with_cell_notes();
        {
            let doc = std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc);
            let mut nested = Table::new(tid, 1, 1, 0, 0, 100.0);
            nested.cell_mut(0, 0).unwrap().text.insert_note(0, "Nested note.", ParaFormat::default());
            let outer = doc.text_story_mut(sid, Some(CellAddr { table: tid, row: 0, col: 0 })).unwrap();
            let end = outer.len();
            outer.insert_table(end, nested);
        }
        let l = s.execute("footnote.list", &json!({})).unwrap();
        let texts: Vec<&str> = l.as_array().unwrap().iter().filter_map(|n| n["text"].as_str()).collect();
        assert!(!texts.contains(&"Nested note."), "{texts:?}");
        assert!(texts.contains(&"Note A (left cell, row 1)."), "{texts:?}");
    }

    #[test]
    fn table_cells_are_read_from_each_rows_first_column() {
        let mut t = Table::new(1, 2, 2, 0, 0, 100.0);
        assert_eq!(cell_reading_order(&t), [(0, 0), (0, 1), (1, 0), (1, 1)]);
        // A right-to-left table lays column 0 out on the right, so it is still read first.
        t.options.direction = designcraft_doc::TextDirection::RightToLeft;
        assert_eq!(cell_reading_order(&t), [(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    #[test]
    fn list_finds_cell_notes_when_the_story_has_no_body_notes() {
        let (mut s, sid, tid) = session_with_cell_notes();
        let ids: Vec<u64> = s.doc().unwrap().doc.story(sid).unwrap().notes.iter().map(|n| n.id).collect();
        for id in ids {
            s.execute("footnote.delete", &json!({"story": sid.0, "id": id})).unwrap();
        }
        let l = s.execute("footnote.list", &json!({})).unwrap();
        let notes = l.as_array().unwrap();
        assert_eq!(notes.len(), 3, "the three cell notes remain, with the empty cell contributing none");
        assert!(notes.iter().all(|n| n["table"] == tid));
    }

    #[test]
    fn a_table_cell_footnote_can_be_edited_and_deleted_by_its_listed_cell() {
        let (mut s, sid, tid) = session_with_cell_notes();
        let notes = s.execute("footnote.list", &json!({})).unwrap();
        let id = notes[3]["id"].as_u64().unwrap();
        let cell = CellAddr { table: tid, row: 1, col: 1 };
        // A cell note's id is only unique within its story: the cell fields pick it out.
        assert!(s.execute("footnote.setText", &json!({"story": sid.0, "id": id, "table": tid, "text": "x"})).is_err());
        s.execute("footnote.setText", &json!({"story": sid.0, "id": id, "table": tid, "row": 1, "col": 1, "text": "Changed."})).unwrap();
        assert_eq!(s.doc().unwrap().doc.text_story(sid, Some(cell)).unwrap().notes[0].text.text, "Changed.");
        assert_eq!(s.doc().unwrap().doc.story(sid).unwrap().notes[0].text.text, "Note 1 (body).");
        s.execute("footnote.delete", &json!({"story": sid.0, "id": id, "table": tid, "row": 1, "col": 1})).unwrap();
        let d = s.doc().unwrap();
        let cell_story = d.doc.text_story(sid, Some(cell)).unwrap();
        assert!(cell_story.notes.is_empty());
        assert!(!cell_story.text.contains(designcraft_doc::FOOTNOTE_REF));
        assert_eq!(d.doc.story(sid).unwrap().notes.len(), 2, "the body notes stay");
        assert_eq!(d.doc.story(sid).unwrap().text.matches(designcraft_doc::FOOTNOTE_REF).count(), 2);
    }

    #[test]
    fn cell_note_edits_leave_a_body_note_caret_unchanged() {
        let (mut s, sid, tid) = session_with_cell_notes();
        let id = s.doc().unwrap().doc.story(sid).unwrap().notes[0].id;
        let caret = TextSel { cell: Some(CellAddr::footnote(id)), ..TextSel::caret(sid, 4) };
        s.doc_mut().unwrap().selection.text = Some(caret);
        s.execute("footnote.setText", &json!({"story": sid.0, "id": id, "table": tid, "row": 0, "col": 0, "text": "x"})).unwrap();
        assert_eq!(s.doc().unwrap().selection.text, Some(caret));
        s.execute("footnote.delete", &json!({"story": sid.0, "id": id, "table": tid, "row": 0, "col": 0})).unwrap();
        assert_eq!(s.doc().unwrap().selection.text, Some(caret));
    }

    #[test]
    fn deleting_a_cell_note_adjusts_only_a_selection_in_that_cell() {
        for same_cell in [true, false] {
            let (mut s, sid, tid) = session_with_cell_notes();
            let cell = CellAddr { table: tid, row: 0, col: 0 };
            let st = s.doc().unwrap().doc.text_story(sid, Some(cell)).unwrap();
            let id = st.notes[0].id;
            let end = st.len();
            let selected = CellAddr { col: usize::from(!same_cell), ..cell };
            s.doc_mut().unwrap().selection.text = Some(TextSel { cell: Some(selected), ..TextSel::caret(sid, end) });
            s.execute("footnote.delete", &json!({"story": sid.0, "id": id, "table": tid, "row": 0, "col": 0})).unwrap();
            let t = s.doc().unwrap().selection.text.unwrap();
            let expected = if same_cell { end - designcraft_doc::FOOTNOTE_REF.len_utf8() } else { end };
            assert_eq!((t.anchor, t.focus, t.cell), (expected, expected, Some(selected)));
        }
    }

    #[test]
    fn partial_cell_addresses_cannot_change_a_body_note() {
        let (mut s, sid, _) = session_with_cell_notes();
        let id = s.doc().unwrap().doc.story(sid).unwrap().notes[0].id;
        for fields in [json!({"row": 0}), json!({"col": 0}), json!({"table": null, "row": 0, "col": 0})] {
            let mut p = json!({"story": sid.0, "id": id, "text": "changed"});
            merge(&mut p, &fields);
            assert!(s.execute("footnote.setText", &p).is_err());
            assert!(s.execute("footnote.delete", &p).is_err());
        }
        assert_eq!(s.doc().unwrap().doc.story(sid).unwrap().notes[0].text.text, "Note 1 (body).");
    }
}

#[cfg(test)]
mod series_tests {
    use super::*;

    #[test]
    fn new_series_api_defaults_to_splitting_and_honors_explicit_false() {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        assert!(s.doc().unwrap().doc.footnote_options.allow_split);
        for (name, explicit, expected) in [("Default", None, true), ("Unsplit", Some(false), false), ("Split", Some(true), true)] {
            let mut params = json!({"name": name});
            if let Some(value) = explicit {
                params["allowSplit"] = json!(value);
            }
            let id = s.execute("footnote.series.create", &params).unwrap()["id"].as_u64().unwrap();
            let options = s.execute("footnote.series.options", &json!({"series": id, "separator": " "})).unwrap();
            assert_eq!(options["allowSplit"], expected);
        }
    }

    #[test]
    fn series_commands_insert_filter_delete_and_undo() {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        let frame = s.execute("frame.create", &json!({"rect":[72,72,400,600],"content":"text","text":"Body"})).unwrap();
        let fid = designcraft_doc::ItemId(frame["id"].as_u64().unwrap());
        let sid = s.doc().unwrap().doc.item(fid).unwrap().text_frame().unwrap().story;
        let series = s.execute("footnote.series.create", &json!({"name":"Sources","style":"lowerLetters"})).unwrap()["id"].as_u64().unwrap();
        s.execute("footnote.series.options", &json!({"series":series,"placement":{"kind":"margin","side":"outside","gap":8,"width":72}})).unwrap();
        s.execute("text.select", &json!({"story":sid.0,"anchor":4,"focus":4})).unwrap();
        s.execute("footnote.insert", &json!({"series":series,"text":"Source"})).unwrap();
        assert_eq!(s.execute("footnote.list", &json!({"series":series})).unwrap()[0]["series"], series);
        assert_eq!(s.execute("footnote.series.list", &json!({})).unwrap().as_array().unwrap().len(), 2);
        assert!(s.execute("footnote.series.delete", &json!({"series":0})).is_err());
        assert!(s.execute("footnote.series.options", &json!({"series":series,"placement":{"width":0}})).is_err());
        s.execute("footnote.series.delete", &json!({"series":series})).unwrap();
        assert_eq!(s.doc().unwrap().doc.story(sid).unwrap().notes[0].series, 0);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.doc().unwrap().doc.story(sid).unwrap().notes[0].series, series);
        assert!(s.execute("footnote.insert", &json!({"series":999})).is_err());
    }
}

#[cfg(test)]
mod margin_default_tests {
    use super::*;
    #[test]
    fn omitted_margin_side_defaults_to_right() {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        let o = s.execute("footnote.options", &json!({"placement":{"kind":"margin","gap":8,"width":72}})).unwrap();
        assert_eq!(o["placement"]["side"], "right");
        s.execute("footnote.series.create", &json!({"name":"Sources","placement":{"kind":"margin","gap":8,"width":72}})).unwrap();
        let all = s.execute("footnote.series.list", &json!({})).unwrap();
        assert_eq!(all[1]["placement"]["side"], "right");
    }
}

#[cfg(test)]
mod api_split_default_tests {
    use super::*;

    #[test]
    fn new_api_options_default_to_split_and_honor_explicit_false() {
        assert!(read_options(json!({}), "test").unwrap().allow_split);
        assert!(read_options(json!({"allowSplit": true}), "test").unwrap().allow_split);
        assert!(!read_options(json!({"allowSplit": false}), "test").unwrap().allow_split);
    }
}

#[cfg(test)]
mod nested_series_delete_tests {
    use super::*;

    #[test]
    fn deleting_and_recreating_a_series_cannot_rebind_embedded_notes() {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        let frame = s.execute("frame.create", &json!({"rect":[72,72,400,600],"content":"text","text":"Body"})).unwrap();
        let sid = StoryId(frame["story"].as_u64().unwrap());
        let series = s.execute("footnote.series.create", &json!({"name":"Sources"})).unwrap()["id"].as_u64().unwrap();
        let (table_id, outer_id, endnote_id) = s
            .edit(|d, _| {
                let table_id = d.alloc();
                let st = d.story_mut(sid).unwrap();
                let outer = st.insert_note(0, "Outer", Default::default()).unwrap();
                let body = &mut st.note_mut(outer).unwrap().text;
                let nested = body.insert_note(0, "Nested source", Default::default()).unwrap();
                body.note_mut(nested).unwrap().series = series;
                let mut table = designcraft_doc::Table::new(table_id, 1, 1, 0, 0, 240.0);
                let cell = &mut table.cells[0].text;
                let note = cell.insert_note(0, "Cell source", Default::default()).unwrap();
                cell.note_mut(note).unwrap().series = series;
                let endnote = cell.insert_endnote(cell.len(), "Endnote body", Default::default()).unwrap();
                let body = &mut cell.endnote_mut(endnote).unwrap().text;
                let note = body.insert_note(0, "Source in endnote", Default::default()).unwrap();
                body.note_mut(note).unwrap().series = series;
                st.insert_table(0, table);
                Ok((table_id, outer, endnote))
            })
            .unwrap();
        let inspect = |d: &designcraft_doc::Document| {
            let st = d.story(sid).unwrap();
            let cell = &st.tables[&table_id].cells[0].text;
            let notes = [&cell.notes[0], &st.note(outer_id).unwrap().text.notes[0], &cell.endnote(endnote_id).unwrap().text.notes[0]];
            notes.map(|note| (note.series, note.text.text.clone()))
        };
        let original = inspect(&s.doc().unwrap().doc);
        assert!(original.iter().all(|(id, _)| *id == series));
        s.execute("footnote.series.delete", &json!({"series":series})).unwrap();
        let fresh = s.execute("footnote.series.create", &json!({"name":"Unrelated"})).unwrap()["id"].as_u64().unwrap();
        assert_eq!(fresh, series, "the library may reuse the deleted numeric id");
        let reset = inspect(&s.doc().unwrap().doc);
        for ((id, text), (_, before)) in reset.iter().zip(&original) {
            assert_eq!(*id, 0, "embedded note must not join the unrelated new series");
            assert_eq!(text, before);
        }
        s.execute("edit.undo", &json!({})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(inspect(&s.doc().unwrap().doc), original);
        assert_eq!(s.doc().unwrap().doc.note_series.iter().find(|s| s.id == series).unwrap().name, "Sources");
    }
}
