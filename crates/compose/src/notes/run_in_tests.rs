use super::*;
use designcraft_doc::{NotePlacement, NoteStream, ParaFormat, SpreadRef, TextDirection};

fn sample(placement: NotePlacement) -> (Document, designcraft_doc::StoryId) {
    let mut doc = Document::new(&Default::default());
    let (_, sid) =
        doc.add_text_frame(SpreadRef::Doc(0), Rect::new(36.0, 36.0, 300.0, 450.0), doc.default_layer(), "Main text", Default::default()).unwrap();
    doc.footnote_options.placement = placement;
    doc.footnote_options.run_in = true;
    doc.footnote_options.run_in_separator = " • ".into();
    doc.footnote_options.separator = " ".into();
    doc.footnote_options.space_between = 25.0;
    let story = doc.story_mut(sid).unwrap();
    for text in ["Alpha", "Beta", "Gamma"] {
        story.insert_note(story.len(), text, ParaFormat::default()).unwrap();
    }
    (doc, sid)
}

fn notes(cs: &ComposedStory) -> Vec<&PlacedNote> {
    cs.frames.iter().flat_map(|f| &f.notes).collect()
}

fn baseline(n: &PlacedNote) -> f64 {
    n.origin.y + n.text.frames[0].lines[0].baseline
}

#[test]
fn hidden_run_in_body_keeps_separator_without_numbers() {
    for placement in [NotePlacement::ColumnFoot, NotePlacement::Stream] {
        let (mut doc, sid) = sample(placement.clone());
        doc.footnote_options.body_hidden = true;
        doc.footnote_options.ref_hidden = true;
        doc.footnote_options.run_in_separator = " | ".into();
        if placement == NotePlacement::Stream {
            let (route, _) =
                doc.add_text_frame(SpreadRef::Doc(0), Rect::new(320.0, 36.0, 580.0, 450.0), doc.default_layer(), "", Default::default()).unwrap();
            doc.item_mut(route).unwrap().text_frame_mut().unwrap().options.note_stream =
                Some(NoteStream { story: sid, series: 0, follow_inset: None });
        }
        let cs = crate::compose_story(&doc, sid, &Default::default());
        let ns = notes(&cs);
        assert_eq!(ns.len(), 3);
        let text: String =
            ns.iter().flat_map(|n| n.text.frames.iter().flat_map(|f| &f.lines).flat_map(|l| &l.glyphs)).map(|g| g.rendered_char).collect();
        assert!(text.contains('|'), "{text}");
        assert!(!text.chars().any(|c| c.is_ascii_digit()), "{text}");
    }
}

#[test]
fn run_in_notes_share_a_line_but_keep_anchors_and_editing() {
    let (mut doc, sid) = sample(NotePlacement::ColumnFoot);
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let ns = notes(&cs);
    assert_eq!(ns.len(), 3);
    assert!((baseline(ns[0]) - baseline(ns[2])).abs() < 0.01);
    assert!(ns[1].text.frames[0].lines[0].glyphs.iter().any(|g| g.rendered_char == '•'));
    assert!(ns[0].text.frames[0].lines[0].first_in_para);
    assert!(!ns[1].text.frames[0].lines[0].first_in_para);
    for (k, n) in ns.iter().enumerate() {
        assert_eq!(n.index, k);
        assert_eq!(doc.story(sid).unwrap().note_anchor(n.id), Some(9 + k * FOOTNOTE_REF.len_utf8()));
        assert!(n.reference_baseline > 0.0);
        let (fi, x, y, _, _) = note_caret(&cs, n.id, 1).unwrap();
        assert_eq!(hit_note(&cs, fi, Point::new(x + 0.1, y - 1.0)).unwrap().0, n.id);
        assert_eq!(find_note_part(&cs, n.id, 1).unwrap().1.id, n.id);
    }
    doc.footnote_options.run_in = false;
    let separate = crate::compose_story(&doc, sid, &Default::default());
    let separate = notes(&separate);
    assert!(baseline(separate[2]) - baseline(separate[0]) > 50.0);
}

#[test]
fn run_in_false_starts_a_paragraph_and_true_overrides_the_series() {
    let (mut doc, sid) = sample(NotePlacement::ColumnFoot);
    let ids: Vec<_> = doc.story(sid).unwrap().notes.iter().map(|n| n.id).collect();
    doc.story_mut(sid).unwrap().note_mut(ids[1]).unwrap().run_in = Some(false);
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let ns = notes(&cs);
    assert!(baseline(ns[1]) > baseline(ns[0]) + 25.0);
    assert!((baseline(ns[1]) - baseline(ns[2])).abs() < 0.01);
    doc.footnote_options.run_in = false;
    doc.story_mut(sid).unwrap().note_mut(ids[2]).unwrap().run_in = Some(true);
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let ns = notes(&cs);
    assert!((baseline(ns[1]) - baseline(ns[2])).abs() < 0.01);
}

#[test]
fn run_in_separator_never_starts_a_line() {
    // Sweep the shared line's fill so some length puts a break right at the separator.
    for n in 1..60 {
        let (mut doc, sid) = sample(NotePlacement::ColumnFoot);
        let ids: Vec<_> = doc.story(sid).unwrap().notes.iter().map(|n| n.id).collect();
        let note = doc.story_mut(sid).unwrap().note_mut(ids[1]).unwrap();
        note.text = Story::with_text(note.text.id, &"Wi ".repeat(n), ParaFormat::default());
        let cs = crate::compose_story(&doc, sid, &Default::default());
        let ns = notes(&cs);
        let previous_last = ns[1].origin.y + ns[1].text.frames[0].lines.last().unwrap().baseline;
        let inline = (baseline(ns[2]) - previous_last).abs() < 0.01;
        for (i, line) in ns[2].text.frames.iter().flat_map(|f| &f.lines).enumerate() {
            // Only a first line that continues the previous note's line may start mid-line.
            if i == 0 && inline {
                continue;
            }
            let Some(first) = line.glyphs.iter().find(|g| g.adv > 0.0) else { continue };
            assert!(!first.rendered_char.is_whitespace() && first.rendered_char != '•', "n={n}: line starts with {:?}", first.rendered_char);
        }
    }
}

#[test]
fn run_in_stream_frame_and_new_area() {
    let (mut doc, sid) = sample(NotePlacement::Stream);
    let (route, _) =
        doc.add_text_frame(SpreadRef::Doc(0), Rect::new(320.0, 36.0, 580.0, 450.0), doc.default_layer(), "", Default::default()).unwrap();
    doc.item_mut(route).unwrap().text_frame_mut().unwrap().options.note_stream = Some(NoteStream { story: sid, series: 0, follow_inset: None });
    let areas = crate::stream_frame_specs(&doc, sid);
    let opts = ComposeOptions::default();
    let story = doc.story(sid).unwrap();
    let fit = fit_stream_page(
        &doc,
        story,
        StreamPageInput { page: 0, main: 0..story.len(), areas: &areas, force_starts: None },
        &StreamState::default(),
        &opts,
    );
    assert!(fit.starts_fit);
    let ns = &fit.frames[0].notes;
    assert_eq!(ns.len(), 3);
    assert!((baseline(&ns[0]) - baseline(&ns[2])).abs() < 0.01);
    let cut = story.note_anchor(story.notes[1].id).unwrap();
    let first =
        fit_stream_page(&doc, story, StreamPageInput { page: 0, main: 0..cut, areas: &areas, force_starts: None }, &StreamState::default(), &opts);
    let second =
        fit_stream_page(&doc, story, StreamPageInput { page: 1, main: cut..story.len(), areas: &areas, force_starts: None }, &first.state, &opts);
    assert!(second.frames[0].notes[0].text.frames[0].lines[0].first_in_para);
    assert!((baseline(&second.frames[0].notes[0]) - baseline(&second.frames[0].notes[1])).abs() < 0.01);
}

#[test]
fn run_in_margin_respects_the_reference_baseline() {
    let (mut doc, sid) = sample(NotePlacement::Margin { side: designcraft_doc::MarginSide::Right, gap: 10.0, width: 160.0 });
    let story = doc.story_mut(sid).unwrap();
    let at = story.note_anchor(story.notes[1].id).unwrap();
    story.insert(at, "\nNext body line");
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let ns = notes(&cs);
    assert_eq!(ns.len(), 3);
    assert!(baseline(ns[1]) >= ns[1].reference_baseline - 0.01);
    assert!(baseline(ns[1]) > baseline(ns[0]) + 1.0);
    assert!(ns[1].text.frames[0].lines[0].first_in_para);
    assert!((baseline(ns[1]) - baseline(ns[2])).abs() < 0.01);
}

#[test]
fn run_in_margin_group_stays_together_in_the_page_stack() {
    let (doc, sid) = sample(NotePlacement::Margin { side: designcraft_doc::MarginSide::Right, gap: 10.0, width: 220.0 });
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let ns = notes(&cs);
    assert_eq!(ns.len(), 3);
    assert!((baseline(ns[0]) - baseline(ns[1])).abs() < 0.01);
    assert!((baseline(ns[1]) - baseline(ns[2])).abs() < 0.01);
}

#[test]
fn run_in_margin_overflow_is_reported_per_note() {
    let (mut doc, sid) = sample(NotePlacement::Margin { side: designcraft_doc::MarginSide::Right, gap: 10.0, width: 220.0 });
    let story = doc.story_mut(sid).unwrap();
    let id = story.notes[2].id;
    story.note_mut(id).unwrap().text = Story::with_text(designcraft_doc::StoryId(0), &"Long commentary. ".repeat(600), Default::default());
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let ns = notes(&cs);
    assert!((baseline(ns[0]) - baseline(ns[1])).abs() < 0.01);
    assert!(!ns[0].overflow);
    assert!(!ns[1].overflow);
    assert!(ns[2].overflow);
}

#[test]
fn run_in_suppresses_paragraph_starts_and_preserves_later_paragraphs() {
    let (mut doc, sid) = sample(NotePlacement::ColumnFoot);
    let ids: Vec<_> = doc.story(sid).unwrap().notes.iter().map(|n| n.id).collect();
    let note = doc.story_mut(sid).unwrap().note_mut(ids[1]).unwrap();
    note.text.paras[0].para.first_line_indent = Some(100.0);
    note.text.paras[0].para.space_before = Some(80.0);
    note.text.paras[0].para.drop_cap_lines = Some(3);
    note.text.paras[0].para.drop_cap_chars = Some(1);
    note.text.insert(note.text.len(), "\nLater paragraph");
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let ns = notes(&cs);
    assert!((baseline(ns[0]) - baseline(ns[1])).abs() < 0.01);
    let lines = &ns[1].text.frames[0].lines;
    assert!(lines[0].drop_cap.is_none());
    assert!(lines.iter().skip(1).any(|line| line.first_in_para));
}

#[test]
fn run_in_does_not_weaken_explicit_keeps() {
    let (mut doc, sid) = sample(NotePlacement::ColumnFoot);
    let joined = crate::compose_story(&doc, sid, &Default::default());
    assert!((baseline(notes(&joined)[0]) - baseline(notes(&joined)[1])).abs() < 0.01);
    let id = doc.story(sid).unwrap().notes[1].id;
    doc.story_mut(sid).unwrap().note_mut(id).unwrap().text.paras[0].para.keep_lines_together = Some(true);
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let ns = notes(&cs);
    assert!(baseline(ns[1]) > baseline(ns[0]) + 25.0);
    assert!(ns[1].text.frames[0].lines[0].first_in_para);
}

#[test]
fn run_in_keeps_table_anchor_paragraphs_separate() {
    let (mut doc, sid) = sample(NotePlacement::ColumnFoot);
    let joined = crate::compose_story(&doc, sid, &Default::default());
    assert!((baseline(notes(&joined)[0]) - baseline(notes(&joined)[1])).abs() < 0.01);
    let id = doc.story(sid).unwrap().notes[1].id;
    doc.story_mut(sid).unwrap().note_mut(id).unwrap().text.insert_table(0, designcraft_doc::Table::new(1, 1, 1, 0, 0, 100.0));
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let note = find_note(&cs, id).unwrap().1;
    assert!(!note.text.frames[0].tables.is_empty());
    assert!(note.text.frames[0].lines[0].first_in_para);
}

#[test]
fn run_in_hebrew_flows_from_the_right() {
    let (mut doc, sid) = sample(NotePlacement::ColumnFoot);
    let story = doc.story_mut(sid).unwrap();
    for n in &mut story.notes {
        let n = Arc::make_mut(n);
        n.text = Story::with_text(n.text.id, "פירוש קצר", ParaFormat::default());
        n.text.paras[0].para.direction = Some(TextDirection::RightToLeft);
        n.text.paras[0].para.align = Some(designcraft_doc::Align::Right);
    }
    let cs = crate::compose_story(&doc, sid, &Default::default());
    let ns = notes(&cs);
    assert!((baseline(ns[0]) - baseline(ns[1])).abs() < 0.01);
    let x = |n: &PlacedNote| n.text.frames[0].lines[0].glyphs.iter().filter(|g| g.len > 0).map(|g| g.x).fold(f64::INFINITY, f64::min);
    assert!(x(ns[1]) < x(ns[0]));
}

#[test]
fn run_in_long_note_splits_and_keeps_source_ranges() {
    let (mut doc, sid) = sample(NotePlacement::Stream);
    let story = doc.story_mut(sid).unwrap();
    let id = story.notes[1].id;
    story.note_mut(id).unwrap().text = Story::with_text(designcraft_doc::StoryId(0), &"A long comment with words. ".repeat(30), Default::default());
    let (route, _) = doc.add_text_frame(SpreadRef::Doc(0), Rect::new(320.0, 36.0, 580.0, 96.0), doc.default_layer(), "", Default::default()).unwrap();
    doc.item_mut(route).unwrap().text_frame_mut().unwrap().options.note_stream = Some(NoteStream { story: sid, series: 0, follow_inset: None });
    let areas = crate::stream_frame_specs(&doc, sid);
    let story = doc.story(sid).unwrap();
    let end = story.note_anchor(story.notes[2].id).unwrap();
    let fit = fit_stream_page(
        &doc,
        story,
        StreamPageInput { page: 0, main: 0..end, areas: &areas, force_starts: None },
        &StreamState::default(),
        &Default::default(),
    );
    assert!(fit.starts_fit);
    let first = fit.frames[0].notes.iter().find(|n| n.id == id).unwrap();
    assert!((baseline(first) - baseline(&fit.frames[0].notes[0])).abs() < 0.01);
    let cut = first.text.frames[0].range.end;
    assert!(cut > 0 && cut < story.note(id).unwrap().text.len());
    let mut state = fit.state;
    let mut next = cut;
    for page in 1..30 {
        let fit =
            fit_stream_page(&doc, story, StreamPageInput { page, main: end..end, areas: &areas, force_starts: None }, &state, &Default::default());
        for part in fit.frames.iter().flat_map(|f| &f.notes).filter(|n| n.id == id) {
            assert_eq!(part.text.frames[0].range.start, next);
            next = part.text.frames[0].range.end;
        }
        state = fit.state;
        if state.pending_series().next().is_none() {
            break;
        }
    }
    assert_eq!(next, story.note(id).unwrap().text.len());
}
