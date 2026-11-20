//! Footnote layout: numbering, room at the bottom of columns, and placement.
//!
//! When a line holding footnote references is set, each referenced footnote is composed at the
//! column width and the column's text area shrinks by its height (plus the minimum space before
//! the first footnote and the space between footnotes). A line that would collide moves to the
//! next column with its footnotes. After the story is laid out the footnotes of each column are
//! stacked against the column bottom, with the separator rule above the first one.
//!
//! The footnotes of a column read as one text: the first footnote's first baseline sits at the
//! First Baseline offset below the top of the footnote area, and each later footnote's first
//! baseline sits its first line's leading (plus Space Between Footnotes) below the previous
//! footnote's last baseline. Only the last footnote's descent counts toward the area's height.
//!
//! With Span Footnotes Across Columns the notes of a frame share one area across all its columns,
//! composed at the frame's text width: every column of the frame makes room for it.

mod columns;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use designcraft_doc::notes::NoteRestart;
use designcraft_doc::{Document, FOOTNOTE_REF, Footnote, ItemId, Story, TextFrameOptions};
use designcraft_geom::{Point, Rect};

use crate::shape::{Glyph, SubstCtx};
use crate::{ComposeOptions, ComposedStory, Deco, FrameSpec, FrameText, PlacedNote};

/// Where a placed footnote comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum NoteSource {
    /// Index in the composed story's own [`Story::notes`].
    Story(usize),
    /// A table cell's footnote: its table, the owner cell, its place among the notes the cell's
    /// composition shows (`slot`, which tells a nested table's note from the cell's own), and its
    /// index in the story that owns it (`k`).
    Cell { table: u64, row: usize, col: usize, slot: usize, k: usize },
}

/// A footnote referenced inside a table cell, placed at the foot of the column that takes its
/// row (unlike a story's own footnotes, which come from [`Notes::refs_in`]).
#[derive(Clone)]
pub(crate) struct CellNote {
    pub(crate) source: NoteSource,
    /// The number the cell's own composition shows.
    pub(crate) label: String,
    pub(crate) note: Arc<Footnote>,
}
pub(crate) mod streams;
pub use streams::{StreamPageFit, StreamPageInput, StreamState, fit_stream_page};
pub(crate) use streams::{fit_stream_page_dynamic, fit_stream_page_for};

#[cfg(test)]
thread_local! {
    static ORIGIN_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ORIGIN_LAST: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
    static ORIGIN_RECHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A footnote assigned to a column.
pub(crate) struct Placed {
    part: usize,
    source: NoteSource,
    note: Arc<Footnote>,
    label: String,
    fi: usize,
    col: usize,
    m: NoteMetrics,
    text: Arc<ComposedStory>,
}

/// Vertical metrics of a composed footnote, relative to the top of its composed text.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NoteMetrics {
    /// Note owning the paragraph format at the end of this part.
    owner: usize,
    /// First and last baselines (the first one includes the First Baseline offset).
    pub(super) first: f64,
    pub(super) last: f64,
    /// First baseline's distance below the previous note's last, before Space Between.
    /// Run-in cancels that space here; a shared baseline uses `-space_between`.
    pub(super) lead: f64,
    /// Descent of the last line.
    pub(super) descent: f64,
    ascent: f64,
    cap: Option<(f64, f64)>,
}

impl NoteMetrics {
    fn from_frame(frame: &FrameText, owner: usize, run_in: Option<&RunIn>, space_between: f64, first_part: bool) -> Option<Self> {
        let a = frame.lines.first()?;
        let z = frame.lines.last()?;
        let cap = frame.lines.iter().filter_map(|l| l.drop_cap).map(|c| (c.rect.y0, c.rect.y1)).reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)));
        Some(Self {
            owner: if z.para == 0 { run_in.map_or(owner, |run| run.owner) } else { owner },
            first: a.baseline,
            last: z.baseline,
            lead: if first_part { run_in.map_or(a.leading, |run| if run.inline { 0.0 } else { a.leading } - space_between) } else { a.leading },
            ascent: a.ascent,
            descent: cap.map_or(z.descent, |(_, bottom)| z.descent.max(bottom - z.baseline)),
            cap,
        })
    }

    /// Obstacles and final placement must share the resolved run-in and drop-cap origin.
    fn origin(self, top: f64, previous: Option<(f64, Self)>, space_before: f64, space_between: f64) -> f64 {
        previous
            .map_or(top + space_before + self.top_padding(), |(baseline, metrics)| baseline + space_between + metrics.advance_to(self) - self.first)
    }

    fn top_padding(self) -> f64 {
        self.cap.map_or(0.0, |(top, _)| (-top).max(0.0))
    }

    /// Preserve normal baseline spacing, adding room only for overhanging drop-cap ink.
    fn advance_to(self, next: Self) -> f64 {
        let mut step = next.lead;
        if let Some((_, bottom)) = self.cap {
            step = step.max(bottom - self.last + next.ascent);
        }
        if let Some((top, _)) = next.cap {
            step = step.max(self.descent + next.first - top);
        }
        step
    }
}

#[derive(Clone, Copy)]
pub(crate) struct NoteColumn<'a> {
    pub(crate) frame_index: usize,
    pub(crate) column_index: usize,
    pub(crate) width: f64,
    pub(crate) frame: &'a FrameSpec,
}

/// The page, spread or section a reference on absolute page `page` counts in (none when numbering
/// never restarts, or when the frame is on no page).
pub(crate) fn restart_key(doc: &Document, page: Option<usize>) -> Option<usize> {
    let p = page?;
    match doc.footnote_options.restart {
        NoteRestart::Never => None,
        NoteRestart::Page => Some(p),
        NoteRestart::Spread => doc.page_loc(p).map(|l| l.0),
        NoteRestart::Section => Some(doc.section_of(p).map_or(0, |s| s.start)),
    }
}

/// Height of footnotes stacked baseline to baseline, from the top of the footnote area to the
/// last footnote's descent.
fn stack_height<'a>(notes: impl IntoIterator<Item = &'a NoteMetrics>, space_between: f64) -> f64 {
    let mut h: Option<(f64, NoteMetrics)> = None;
    let mut descent = 0.0;
    for m in notes {
        let last = match h {
            None => m.last + m.top_padding(),
            Some((prev, metrics)) => prev + space_between + metrics.advance_to(*m) + (m.last - m.first),
        };
        h = Some((last, *m));
        descent = m.descent;
    }
    h.map_or(0.0, |(h, _)| h + descent)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct PartPage {
    page_name: Option<String>,
    section_marker: Option<String>,
    page: Option<usize>,
    parent: bool,
    left_page: bool,
    right_page: bool,
}

impl PartPage {
    fn of(f: &FrameSpec) -> PartPage {
        PartPage {
            page_name: f.page_name.clone(),
            section_marker: f.section_marker.clone(),
            page: f.page,
            parent: f.parent,
            left_page: f.left_page,
            right_page: f.right_page,
        }
    }
}

/// Last placed part, including each part's area and page context used to thread its text.
#[derive(Clone, Debug)]
pub(crate) struct Carry {
    k: usize,
    heights: Vec<(f64, f64)>,
    pages: Vec<PartPage>,
    continuations: Vec<(usize, f64)>,
    exclusions: Vec<(usize, Vec<crate::Exclusion>)>,
    fi: usize,
    col: usize,
    run_in: Option<RunIn>,
}

/// Only the first paragraph joins the preceding note; later paragraphs keep their formats.
#[derive(Clone, Debug)]
struct RunIn {
    owner: usize,
    indent: f64,
    para: designcraft_doc::ParaFormat,
    inline: bool,
}

pub(crate) struct SplitPlan {
    band_span: Option<f64>,
    exclusions: Vec<(usize, Vec<crate::Exclusion>)>,
    continuations: Vec<(usize, f64)>,
    remainder: bool,
    k: usize,
    heights: Vec<(f64, f64)>,
    pages: Vec<PartPage>,
    text: Arc<ComposedStory>,
    m: NoteMetrics,
    run_in: Option<RunIn>,
}

type SeriesSnapshot = (u32, Option<usize>, usize, Option<Carry>);
type Part = (Arc<ComposedStory>, NoteMetrics);

/// The number a story's next footnote reference takes, and the restart unit it counts in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoteNumber {
    pub num: u32,
    /// The page, spread or section the number counts in (none when numbering never restarts).
    pub key: Option<usize>,
}
type CacheKey = (NoteSource, String, u64, PartPage, Vec<(u64, u64, PartPage)>, Option<(usize, u64, bool)>);

impl NoteNumber {
    /// Continue this count on `page`, restarting when its page, spread or section changes.
    pub fn at_page(self, doc: &Document, page: Option<usize>) -> Self {
        match restart_key(doc, page) {
            Some(key) if self.key != Some(key) => Self { num: doc.footnote_options.start_at, key: Some(key) },
            _ => self,
        }
    }
}

fn append_note(notes: &mut Vec<PlacedNote>, note: PlacedNote) {
    if note.text.frames.first().and_then(|f| f.lines.first()).is_some_and(|l| !l.first_in_para)
        && let Some(previous) = notes.last_mut().filter(|n| n.series == note.series)
        && let Some(line) = Arc::make_mut(&mut previous.text).frames.first_mut().and_then(|f| f.lines.last_mut())
    {
        line.last_in_para = false;
    }
    notes.push(note);
}
/// Shared by final margin placement and inset trials; no gap precedes the first entry.
pub(crate) fn top_margin_entry(top: f64, previous: Option<f64>, height: f64, spacing: f64) -> (f64, f64) {
    let y = previous.map_or(top, |bottom| bottom + spacing);
    (y, y + height)
}

pub(super) struct SeriesNotes {
    series: u64,
    nested: bool,
    direction: designcraft_doc::TextDirection,
    band_previous: Option<(f64, NoteMetrics)>,
    route_first_baseline: Option<f64>,
    band_column: Option<(usize, usize)>,
    band_last: Option<(f64, NoteMetrics)>,
    band_continued: bool,
    band_span: Option<f64>,
    band_spans: HashMap<(usize, usize), f64>,
    band_incoming: Option<(usize, usize)>,
    members: Vec<bool>,
    /// Byte offsets of the reference characters (index = footnote index).
    refs: Vec<usize>,
    /// Number of each footnote (assigned when its paragraph is shaped).
    numbers: Vec<u32>,
    /// Next number and the restart unit (page / spread / section start) it counts in.
    pub num: u32,
    pub key: Option<usize>,
    pub placed: Vec<Placed>,
    /// Composed footnote text by note, label, area dimensions and page context.
    cache: HashMap<CacheKey, Vec<Option<Part>>>,
    pub carry: Option<Carry>,
    notes: Vec<Arc<designcraft_doc::Footnote>>,
    /// Footnotes span the columns of their frame.
    span: bool,
}

/// Whether composition can number footnotes directly or inside a paragraph's table.
pub(crate) fn has_notes_or_tables(story: &Story) -> bool {
    !story.notes.is_empty() || (0..story.paras.len()).any(|pi| story.para_table(pi).is_some())
}

impl SeriesNotes {
    pub(super) fn new(doc: &Document, story: &Story, series: u64, nested: bool, start: Option<NoteNumber>) -> Self {
        let refs: Vec<usize> = if story.notes.is_empty() { Vec::new() } else { story.text.match_indices(FOOTNOTE_REF).map(|(i, _)| i).collect() };
        let start =
            start.unwrap_or(NoteNumber { num: if has_notes_or_tables(story) { doc.footnote_series_start(story.id, series) } else { 1 }, key: None });
        Self {
            series,
            nested,
            direction: story.direction,
            band_previous: None,
            route_first_baseline: None,
            band_column: None,
            band_last: None,
            band_continued: false,
            band_span: None,
            band_spans: HashMap::new(),
            band_incoming: None,
            members: story.notes.iter().map(|n| nested || doc.note_series_id(n.series) == series).collect(),
            numbers: vec![start.num; refs.len()],
            refs,
            num: start.num,
            key: start.key,
            placed: Vec::new(),
            cache: HashMap::new(),
            carry: None,
            notes: story.notes.clone(),
            span: doc.note_options(series).span_columns,
        }
    }

    fn geometry(&self, doc: &Document, width: f64) -> (usize, f64) {
        let o = doc.note_options(self.series);
        if !self.nested && matches!(o.placement, designcraft_doc::NotePlacement::ColumnFoot) { o.column_geometry(width) } else { (1, width) }
    }

    fn owns(&self, k: usize) -> bool {
        self.members.get(k).copied().unwrap_or(false)
    }

    fn column<'a>(&self, mut column: NoteColumn<'a>) -> NoteColumn<'a> {
        if self.span {
            column.width = column.frame.columns().into_iter().reduce(|a, b| a.union(b)).map_or(column.width, |r| r.width());
        }
        column
    }

    /// Whether a note placed in (fi, col) shares the footnote area of column (`fi`, `ci`).
    fn same_area(&self, p: &Placed, fi: usize, ci: usize) -> bool {
        p.fi == fi && (self.span || p.col == ci)
    }

    /// The footnote area of column `col` of a frame with columns `cols`: the column, or all of
    /// them when notes span columns.
    pub fn area(&self, cols: &[Rect], col: usize) -> Option<Rect> {
        if self.span { cols.iter().copied().reduce(|a, b| a.union(b)) } else { cols.get(col.min(cols.len().saturating_sub(1))).copied() }
    }

    pub fn active(&self) -> bool {
        !self.refs.is_empty() && self.refs.len() == self.notes.len()
    }

    /// Number the references in a paragraph (shaped on `page`) and hand their labels to the shaper.
    pub fn number_refs(&mut self, doc: &Document, prange: Range<usize>, page: Option<usize>, sub: &mut SubstCtx) {
        let o = doc.note_options(self.series);
        let key = page.and_then(|p| match o.restart {
            NoteRestart::Never => None,
            NoteRestart::Page => Some(p),
            NoteRestart::Spread => doc.page_loc(p).map(|l| l.0),
            NoteRestart::Section => Some(doc.section_of(p).map_or(0, |s| s.start)),
        });
        let a = self.refs.partition_point(|&b| b < prange.start);
        let b = self.refs.partition_point(|&b| b < prange.end);
        for k in a..b {
            if !self.owns(k) {
                continue;
            }
            if o.restart != NoteRestart::Never && key != self.key {
                self.key = key;
                self.num = o.start_at;
            }
            self.numbers[k] = self.num;
            self.num = self.num.saturating_add(1);
            sub.notes.insert(self.refs[k], if o.ref_hidden { String::new() } else { o.label(self.numbers[k], false) });
            sub.note_formats.insert(self.refs[k], (o.ref_position, o.ref_char_style.clone()));
        }
    }

    /// Footnote indices referenced by a line's glyphs.
    /// In the order they first appear; a set keeps a line with many references linear.
    pub fn refs_in(&self, glyphs: &[Glyph]) -> Vec<usize> {
        let mut v: Vec<usize> = Vec::new();
        let mut seen = HashSet::new();
        for g in glyphs.iter().filter(|g| g.ch == FOOTNOTE_REF && g.len > 0) {
            if let Ok(k) = self.refs.binary_search(&g.byte)
                && self.owns(k)
                && seen.insert(k)
            {
                v.push(k);
            }
        }
        v
    }

    /// New notes keep the preceding baseline; their origin can precede the band's top.
    /// Wrapping obstacles must use this same origin, including for split-part replay.
    fn band_note_origin(&self, doc: &Document, f: &FrameSpec, metrics: NoteMetrics) -> f64 {
        #[cfg(test)]
        ORIGIN_VISITS.with(|n| n.set(n.get() + 1));
        let o = doc.note_options(self.series);
        let previous = self.band_last.or(self.band_previous);
        metrics.origin(f.area.y0, previous, o.space_before, o.space_between)
    }

    fn compose(&mut self, doc: &Document, k: usize, w: f64, f: &FrameSpec, opts: &ComposeOptions) -> Part {
        self.compose_parts(doc, k, w, f, opts, &[], &[])
            .first()
            .and_then(Clone::clone)
            .unwrap_or_else(|| (Arc::new(ComposedStory::default()), NoteMetrics::default()))
    }

    /// One threaded composition lets the note's own paragraph keeps govern each frame break.
    /// `heights` and `pages` hold each earlier part's area and page context; the last part is `w` wide and unbounded.
    #[allow(clippy::too_many_arguments)] // Each earlier area needs its dimensions and page context.
    pub(super) fn compose_parts(
        &mut self,
        doc: &Document,
        k: usize,
        w: f64,
        f: &FrameSpec,
        opts: &ComposeOptions,
        heights: &[(f64, f64)],
        pages: &[PartPage],
    ) -> Vec<Option<Part>> {
        self.compose_parts_in(doc, k, w, f, opts, heights, pages, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn compose_parts_in(
        &mut self,
        doc: &Document,
        k: usize,
        w: f64,
        f: &FrameSpec,
        opts: &ComposeOptions,
        heights: &[(f64, f64)],
        pages: &[PartPage],
        run_in: Option<&RunIn>,
    ) -> Vec<Option<Part>> {
        let Some(note) = self.notes.get(k).cloned() else { return Vec::new() };
        let o = doc.note_options(self.series);
        let label = o.label(self.numbers.get(k).copied().unwrap_or(o.start_at), true);
        self.compose_note_parts(doc, NoteSource::Story(k), &label, &note, (w, f), opts, heights, pages, run_in)
    }

    fn compose_note(
        &mut self,
        doc: &Document,
        source: NoteSource,
        label: &str,
        note: &Footnote,
        area: (f64, &FrameSpec),
        opts: &ComposeOptions,
    ) -> Part {
        self.compose_note_parts(doc, source, label, note, area, opts, &[], &[], None)
            .first()
            .and_then(Clone::clone)
            .unwrap_or_else(|| (Arc::new(ComposedStory::default()), NoteMetrics::default()))
    }

    #[allow(clippy::too_many_arguments)]
    fn compose_note_parts(
        &mut self,
        doc: &Document,
        source: NoteSource,
        body_label: &str,
        note: &Footnote,
        (w, f): (f64, &FrameSpec),
        opts: &ComposeOptions,
        heights: &[(f64, f64)],
        pages: &[PartPage],
        run_in: Option<&RunIn>,
    ) -> Vec<Option<Part>> {
        let k = match source {
            NoteSource::Story(k) | NoteSource::Cell { k, .. } => k,
        };
        let (_, w) = self.geometry(doc, w);
        let o = doc.note_options(self.series);
        // Inline, the separator's leading spaces are no-break so a line never breaks before its
        // mark (or starts with a space). A note moved to a fresh line drops the separator.
        let separator = match run_in {
            Some(run) if run.inline => {
                let mark = o.run_in_separator.trim_start();
                let spaces = o.run_in_separator.len() - mark.len();
                format!("{}{mark}", "\u{a0}".repeat(o.run_in_separator[..spaces].chars().count()))
            }
            Some(_) | None => String::new(),
        };
        let label = if o.body_hidden { separator.clone() } else { format!("{}{}{}", separator, body_label, o.separator) };
        // Parent frames have no page index: their prefix and section context must also distinguish cached text.
        let current_page = PartPage::of(f);
        // Band replays depend on mutable continuation and contour history. Keep the
        // ordinary route cache; band trials must measure their exact current inputs.
        let key = (!opts.note_bands).then(|| {
            (
                source,
                label.clone(),
                w.to_bits(),
                current_page.clone(),
                heights
                    .iter()
                    .enumerate()
                    .map(|(i, (pw, h))| {
                        let page = pages.get(i).unwrap_or(&current_page);
                        (pw.to_bits(), h.to_bits(), page.clone())
                    })
                    .collect(),
                run_in.map(|run| (run.owner, run.indent.to_bits(), run.inline)),
            )
        });
        if let Some(c) = key.as_ref().and_then(|key| self.cache.get(key)) {
            return c.clone();
        }
        let spec = FrameSpec {
            id: ItemId(0),
            area: Rect::new(0.0, 0.0, w.max(1.0), 1.0e6),
            opts: TextFrameOptions { first_baseline: o.first_baseline, first_baseline_min: o.first_baseline_min, ..Default::default() },
            vertical: false,
            exclusions: vec![],
            outline: None,
            page_name: f.page_name.clone(),
            section_marker: f.section_marker.clone(),
            page: f.page,
            parent: f.parent,
            grid: None,
            left_page: f.left_page,
            right_page: f.right_page,
            page_rect: None,
        };
        let nopts = ComposeOptions {
            page_name: opts.page_name.clone(),
            section_marker: opts.section_marker.clone(),
            page: opts.page,
            running: opts.running.clone(),
            label: (!label.is_empty()).then_some(label.clone()),
            xrefs: opts.xrefs.clone(),
            placed: Vec::new(),
            continuations: if matches!(source, NoteSource::Story(_)) { self.part_continuations(k, heights, opts.note_bands) } else { Vec::new() },
            ..Default::default()
        };
        let previous = self.carry.as_ref().filter(|c| matches!(source, NoteSource::Story(_)) && c.k == k);
        let part = previous.map_or(0, |c| c.heights.len());
        let local_exclusions = || {
            if opts.note_bands {
                f.exclusions
                    .iter()
                    .map(|e| crate::Exclusion {
                        rect: e.rect + designcraft_geom::Vec2::new(-f.area.x0, -f.area.y0),
                        contour: e.contour.as_ref().map(|c| c.clone().transformed(designcraft_geom::Affine::translate((-f.area.x0, -f.area.y0)))),
                        ..e.clone()
                    })
                    .collect()
            } else {
                Vec::new()
            }
        };
        let specs: Vec<FrameSpec> = heights
            .iter()
            .copied()
            .chain(std::iter::once((w, 1.0e6)))
            .enumerate()
            .map(|(i, (pw, h))| {
                let exclusions = if i == part {
                    local_exclusions()
                } else {
                    previous.and_then(|c| c.exclusions.iter().find(|(index, _)| *index == i)).map_or_else(Vec::new, |(_, e)| e.clone())
                };
                let page = pages.get(i).unwrap_or(&current_page);
                FrameSpec {
                    area: Rect::new(0.0, 0.0, pw.max(1.0), h),
                    exclusions,
                    page_name: page.page_name.clone(),
                    section_marker: page.section_marker.clone(),
                    page: page.page,
                    parent: page.parent,
                    left_page: page.left_page,
                    right_page: page.right_page,
                    ..spec.clone()
                }
            })
            .collect();
        let mut text = note.text.clone();
        if let Some(run) = run_in
            && let Some(first) = text.paras.first_mut()
        {
            *first = run.para.clone();
            first.para.first_line_indent = Some(run.indent);
            first.para.space_before = Some(0.0);
            first.para.drop_cap_lines = Some(0);
            first.para.drop_cap_chars = Some(0);
            first.para.list_type = Some(designcraft_doc::ListType::None);
        }
        // A replay must test overlap against the first part's original width.
        let first_width = specs.first().map_or(w, |spec| spec.area.width());
        let mut cs = crate::compose(doc, &text, &specs, &nopts);
        if opts.note_bands && part == 0 && !f.exclusions.is_empty() {
            let mut positioned = specs;
            let mut applied = 0.0;
            // A different first-line break can change leading and therefore the note origin.
            for _ in 0..2 {
                let Some(metrics) = cs.frames.first().and_then(|frame| NoteMetrics::from_frame(frame, k, run_in, o.space_between, true)) else {
                    break;
                };
                let origin = self.band_note_origin(doc, f, metrics);
                #[cfg(test)]
                {
                    ORIGIN_LAST.with(|n| n.set(Some(origin)));
                    ORIGIN_RECHECKS.with(|n| n.set(n.get() + 1));
                }
                let desired = f.area.y0 - origin;
                let delta = desired - applied;
                if delta.abs() <= 0.01 {
                    break;
                }
                if let Some(first) = positioned.first_mut() {
                    for exclusion in &mut first.exclusions {
                        exclusion.rect = exclusion.rect + designcraft_geom::Vec2::new(0.0, delta);
                        exclusion.contour =
                            exclusion.contour.as_ref().map(|c| c.clone().transformed(designcraft_geom::Affine::translate((0.0, delta))));
                    }
                }
                cs = crate::compose(doc, &text, &positioned, &nopts);
                applied = desired;
            }
        }
        if let Some(run) = run_in.filter(|run| run.inline)
            && cs.frames.first().and_then(|f| f.lines.first()).is_some_and(|line| {
                let (pp, _) = doc.styles.resolve_para(&run.para);
                let label_advance: f64 = line.glyphs.iter().filter(|g| g.byte == 0 && g.len == 0).map(|g| g.adv).sum();
                line.glyphs.iter().any(|g| {
                    g.x < -0.01
                        || g.x + g.adv > first_width + 0.01
                        || (g.len > 0
                            && if pp.direction == designcraft_doc::TextDirection::RightToLeft {
                                g.x + g.adv > first_width - pp.right_indent - run.indent - label_advance + 0.01
                            } else {
                                g.x < pp.left_indent + run.indent + label_advance - 0.01
                            })
                })
            })
        {
            // An unbreakable first word must move to the next line, never overprint its predecessor.
            let next_line = RunIn { indent: 0.0, inline: false, ..run.clone() };
            return self.compose_note_parts(doc, source, body_label, note, (w, f), opts, heights, pages, Some(&next_line));
        }
        let parts = cs
            .frames
            .iter()
            .enumerate()
            .map(|(i, ft)| {
                let m = NoteMetrics::from_frame(ft, k, run_in, o.space_between, i == 0)?;
                let mut frame = ft.clone();
                // Body frame ranges exclude paragraph separators; parts own those bytes too.
                frame.range.end = cs
                    .frames
                    .iter()
                    .skip(i.saturating_add(1))
                    .find(|f| !f.lines.is_empty())
                    .map_or(cs.overset_at.unwrap_or(note.text.len()), |f| f.range.start);
                if i == 0
                    && run_in.is_some()
                    && let Some(first) = frame.lines.first_mut()
                {
                    first.first_in_para = false;
                }
                let text = ComposedStory { frames: vec![frame], ..cs.clone() };
                Some((Arc::new(text), m))
            })
            .collect::<Vec<_>>();
        if let Some(key) = key {
            self.cache.insert(key, parts.clone());
        }
        parts
    }

    fn part_continuations(&self, k: usize, heights: &[(f64, f64)], bands: bool) -> Vec<(usize, f64)> {
        let mut parts = self.carry.as_ref().filter(|c| c.k == k).map_or_else(Vec::new, |c| c.continuations.clone());
        if bands {
            let first = self.band_incoming.filter(|(index, _)| *index == k).map_or(1, |(_, count)| count.saturating_add(1));
            parts.extend(
                heights
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| i.saturating_add(1) >= first)
                    .map(|(i, (_, h))| (i.saturating_add(1), -self.band_spans.get(&(k, i)).copied().unwrap_or(*h))),
            );
        }
        parts.sort_by_key(|(index, _)| *index);
        parts.dedup_by_key(|(index, _)| *index);
        parts
    }

    fn pending(&self, column: NoteColumn<'_>) -> Option<Carry> {
        self.carry
            .as_ref()
            .filter(|c| column.frame_index > c.fi || (column.frame_index == c.fi && !self.span && column.column_index > c.col))
            .cloned()
    }

    fn remainder(&mut self, doc: &Document, c: &Carry, column: NoteColumn<'_>, opts: &ComposeOptions) -> Option<Part> {
        self.compose_parts_in(doc, c.k, column.width, column.frame, opts, &c.heights, &c.pages, c.run_in.as_ref())
            .get(c.heights.len())
            .and_then(Clone::clone)
    }

    /// Shared by column feet, stream columns and margins. A new area has no predecessor.
    fn run_in(&self, doc: &Document, k: usize, width: f64, previous: Option<(usize, &ComposedStory)>) -> Option<RunIn> {
        let note = self.notes.get(k)?;
        if !note.run_in.unwrap_or(doc.note_options(self.series).run_in) {
            return None;
        }
        let (pk, text) = previous?;
        let line = text.frames.first()?.lines.last()?;
        let previous = self.notes.get(pk)?;
        let para = previous.text.paras.last()?.clone();
        // A table anchor owns its paragraph and its story-local table id.
        if para.table.is_some() || note.text.paras.first()?.table.is_some() {
            return None;
        }
        let (pp, _) = doc.styles.resolve_para(&para);
        let (own, _) = doc.styles.resolve_para(note.text.paras.first()?);
        // Joining must not weaken a note's keeps or its required column/frame/page start.
        if own.start_paragraph != designcraft_doc::StartParagraph::Anywhere
            || own.keep_lines_together
            || pp.keep_lines_together
            || own.keep_with_next > 0
            || pp.keep_with_next > 0
        {
            return None;
        }
        let rtl = pp.direction == designcraft_doc::TextDirection::RightToLeft;
        let end = crate::caret(text, line.range.end).map_or(line.end_x, |(_, x, _, _, _)| x);
        let indent = if rtl { width - end - pp.right_indent } else { end - pp.left_indent };
        // A line with no remaining width advances normally instead of forcing an overfull word.
        if !indent.is_finite() || indent < 0.0 {
            return None;
        }
        let inline = indent < width - pp.left_indent - pp.right_indent - 1.0;
        Some(RunIn { owner: pk, indent: if inline { indent } else { 0.0 }, para, inline })
    }

    fn compose_after(
        &mut self,
        doc: &Document,
        k: usize,
        column: NoteColumn<'_>,
        opts: &ComposeOptions,
        previous: Option<(usize, &ComposedStory)>,
    ) -> Part {
        let run = self.run_in(doc, k, column.width, previous);
        self.compose_with_run(doc, k, column, opts, run.as_ref())
    }

    fn compose_with_run(&mut self, doc: &Document, k: usize, column: NoteColumn<'_>, opts: &ComposeOptions, run: Option<&RunIn>) -> Part {
        self.compose_parts_in(doc, k, column.width, column.frame, opts, &[], &[], run)
            .first()
            .and_then(Clone::clone)
            .unwrap_or_else(|| (Arc::new(ComposedStory::default()), NoteMetrics::default()))
    }

    fn metrics(&mut self, doc: &Document, column: NoteColumn<'_>, extra: &[usize], opts: &ComposeOptions) -> Vec<NoteMetrics> {
        let here: Vec<_> = self.placed.iter().filter(|p| self.same_area(p, column.frame_index, column.column_index)).collect();
        let mut previous = here.last().map(|p| (p.m.owner, p.text.clone()));
        let mut ms: Vec<_> = here.iter().map(|p| p.m).collect();
        if let Some(c) = self.pending(column)
            && let Some((text, m)) = self.remainder(doc, &c, column, opts)
        {
            ms.push(m);
            previous = Some((m.owner, text));
        }
        for &k in extra {
            let (text, m) = self.compose_after(doc, k, column, opts, previous.as_ref().map(|(k, t)| (*k, t.as_ref())));
            ms.push(m);
            previous = Some((m.owner, text));
        }
        ms
    }

    /// Reserve fixed notes first, then use the largest frame height left for the final note.
    pub fn split_plan(
        &mut self,
        doc: &Document,
        column: NoteColumn<'_>,
        extra: &[usize],
        available: f64,
        opts: &ComposeOptions,
    ) -> Option<SplitPlan> {
        if !doc.note_options(self.series).allow_split || !available.is_finite() || available <= 0.0 {
            return None;
        }
        let (count, width) = self.geometry(doc, column.width);
        let prefix =
            if count > 1 { self.area_texts(doc, column, extra.get(..extra.len().saturating_sub(1)).unwrap_or_default(), opts) } else { Vec::new() };
        let pending = self.pending(column);
        let (k, mut heights, mut pages, ms, original, run_in) = if let Some((&k, before)) = extra.split_last() {
            // If the carry must split too, move this referencing line on as before.
            if self.carry.is_some() && pending.is_none() {
                return None;
            }
            let ms = self.metrics(doc, column, before, opts);
            if count == 1 && stack_height(&ms, doc.note_options(self.series).space_between) > available {
                return None;
            }
            // A second carry would discard the first note's remainder.
            if pending.as_ref().is_some_and(|c| self.remainder(doc, c, column, opts).is_none()) {
                return None;
            }
            let mut previous =
                self.placed.iter().rev().find(|p| self.same_area(p, column.frame_index, column.column_index)).map(|p| (p.m.owner, p.text.clone()));
            if let Some(c) = &pending
                && let Some((text, m)) = self.remainder(doc, c, column, opts)
            {
                previous = Some((m.owner, text));
            }
            for &pk in before {
                let (text, m) = self.compose_after(doc, pk, column, opts, previous.as_ref().map(|(k, t)| (*k, t.as_ref())));
                previous = Some((m.owner, text));
            }
            let run = self.run_in(doc, k, column.width, previous.as_ref().map(|(k, t)| (*k, t.as_ref())));
            let original = self.compose_with_run(doc, k, column, opts, run.as_ref());
            (k, Vec::new(), Vec::new(), ms, original, run)
        } else {
            let c = pending?;
            let original = self.remainder(doc, &c, column, opts)?;
            let ms = self.placed.iter().filter(|p| self.same_area(p, column.frame_index, column.column_index)).map(|p| p.m).collect();
            (c.k, c.heights, c.pages, ms, original, c.run_in)
        };
        let offset = ms.last().map_or(0.0, |last| {
            stack_height(&ms, doc.note_options(self.series).space_between) - last.descent
                + doc.note_options(self.series).space_between
                + original.1.lead
                - original.1.first
        });
        let mut h = if count > 1 { available * count as f64 } else { available - offset };
        let part = heights.len();
        heights.push((width, h));
        pages.push(PartPage::of(column.frame));
        // Ordinary text frames fit baselines, but a footnote stack also includes descent.
        // Each retry drops its last line; the unsplit remainder bounds the attempts.
        let attempts = original.0.frames.first().map_or(0, |f| f.lines.len());
        for _ in 0..attempts {
            if !h.is_finite() || h <= 0.0 {
                return None;
            }
            *heights.get_mut(part)? = (width, h);
            let parts = self.compose_parts_in(doc, k, column.width, column.frame, opts, &heights, &pages, run_in.as_ref());
            let (text, m) = parts.get(part).and_then(Clone::clone)?;
            let total = if count == 1 {
                stack_height(ms.iter().chain(std::iter::once(&m)), doc.note_options(self.series).space_between)
            } else {
                let mut texts = prefix.clone();
                // With no new reference, the final prefix item is this carry's unsplit remainder.
                if extra.is_empty() && self.pending(column).is_some() {
                    texts.pop();
                }
                texts.push(text.clone());
                columns::balance(&texts, count, doc.note_options(self.series).space_between).0
            };
            if m.last + m.descent <= h + 0.01 && total <= available + 0.01 {
                let mut exclusions = self.carry.as_ref().filter(|c| c.k == k).map_or_else(Vec::new, |c| c.exclusions.clone());
                if opts.note_bands {
                    let origin = if part == 0 { self.band_note_origin(doc, column.frame, original.1) } else { column.frame.area.y0 };
                    exclusions.push((
                        part,
                        column
                            .frame
                            .exclusions
                            .iter()
                            .map(|e| crate::Exclusion {
                                rect: e.rect + designcraft_geom::Vec2::new(-column.frame.area.x0, -origin),
                                contour: e
                                    .contour
                                    .as_ref()
                                    .map(|c| c.clone().transformed(designcraft_geom::Affine::translate((-column.frame.area.x0, -origin)))),
                                ..e.clone()
                            })
                            .collect(),
                    ));
                }
                return Some(SplitPlan {
                    // A note can start below earlier notes in this band. Its continuation
                    // translates by its own remaining span, not the whole band's height.
                    band_span: self.band_span.map(|_| available - offset),
                    exclusions,
                    continuations: self.part_continuations(k, &heights, opts.note_bands).into_iter().filter(|(index, _)| *index <= part).collect(),
                    remainder: parts.get(heights.len()).is_some_and(Option::is_some),
                    k,
                    heights,
                    pages,
                    text,
                    m,
                    run_in,
                });
            }
            let smaller = h.min(m.last - 0.02);
            if smaller >= h {
                return None;
            }
            h = smaller;
        }
        None
    }

    /// Whether every referenced footnote composes completely at this column's width.
    /// A note with text but no placed part (nothing fits, e.g. a drop word wider than the column)
    /// is unplaceable too: `compose` would hide that behind an empty default story.
    pub fn fit(&mut self, doc: &Document, column: NoteColumn<'_>, ks: &[usize], opts: &ComposeOptions) -> bool {
        ks.iter().all(|&k| match self.compose_parts(doc, k, column.width, column.frame, opts, &[], &[]).first() {
            Some(Some((text, _))) => text.overset_at.is_none(),
            _ => self.notes.get(k).is_none_or(|n| n.text.is_empty()),
        })
    }

    /// Height the footnotes of column (fi, col) take, with `extra` footnotes added.
    pub fn reserve(&mut self, doc: &Document, column: NoteColumn<'_>, extra: &[usize], opts: &ComposeOptions) -> f64 {
        self.reserve_metrics(doc, column, extra, &[], opts)
    }

    fn area_texts(&mut self, doc: &Document, column: NoteColumn<'_>, extra: &[usize], opts: &ComposeOptions) -> Vec<Arc<ComposedStory>> {
        let mut texts: Vec<_> =
            self.placed.iter().filter(|p| self.same_area(p, column.frame_index, column.column_index)).map(|p| p.text.clone()).collect();
        if let Some(c) = self.pending(column)
            && let Some((text, _)) = self.remainder(doc, &c, column, opts)
        {
            texts.push(text);
        }
        for &k in extra {
            texts.push(self.compose(doc, k, column.width, column.frame, opts).0);
        }
        texts
    }

    fn reserve_metrics(&mut self, doc: &Document, column: NoteColumn<'_>, own: &[usize], cells: &[CellNote], opts: &ComposeOptions) -> f64 {
        let o = doc.note_options(self.series);
        let space_between = o.space_between;
        let (count, _) = self.geometry(doc, column.width);
        let mut texts = self.area_texts(doc, column, own, opts);
        let mut ms = self.metrics(doc, column, own, opts);
        for n in cells {
            if self.placed.iter().any(|p| p.source == n.source) {
                continue;
            }
            let (text, m) = self.compose_note(doc, n.source, &n.label, &n.note, (column.width, column.frame), opts);
            texts.push(text);
            ms.push(m);
        }
        if ms.is_empty() {
            0.0
        } else {
            (if count == 1 { stack_height(&ms, space_between) } else { columns::balance(&texts, count, space_between).0 })
                + self.top_space(doc, column, &ms)
        }
    }

    fn top_space(&self, doc: &Document, column: NoteColumn<'_>, ms: &[NoteMetrics]) -> f64 {
        let o = doc.note_options(self.series);
        match (self.band_previous, ms.first()) {
            (Some(_), _) if self.band_continued => 0.0,
            (Some(previous), Some(m)) => m.origin(column.frame.area.y0, Some(previous), o.space_before, o.space_between) - column.frame.area.y0,
            (_, Some(first)) => self.route_first_baseline.map_or(o.space_before, |baseline| baseline - first.first),
            _ => o.space_before,
        }
    }

    fn area_space(&mut self, doc: &Document, column: NoteColumn<'_>, extra: &[usize], opts: &ComposeOptions) -> f64 {
        let ms = self.metrics(doc, column, extra, opts);
        self.top_space(doc, column, &ms)
    }

    pub fn place(&mut self, doc: &Document, k: usize, column: NoteColumn<'_>, opts: &ComposeOptions) {
        let previous =
            self.placed.iter().rev().find(|p| self.same_area(p, column.frame_index, column.column_index)).map(|p| (p.m.owner, p.text.clone()));
        let (text, m) = self.compose_after(doc, k, column, opts, previous.as_ref().map(|(k, t)| (*k, t.as_ref())));
        self.push(doc, k, 0, text, m, column);
    }

    fn push(&mut self, doc: &Document, k: usize, part: usize, text: Arc<ComposedStory>, m: NoteMetrics, column: NoteColumn<'_>) {
        if self.placed.iter().any(|p| p.source == NoteSource::Story(k) && p.part == part) {
            return;
        }
        if self.band_column == Some((column.frame_index, column.column_index)) {
            let y = if part > 0 && self.band_last.is_none() && self.band_previous.is_some() {
                column.frame.area.y0
            } else {
                self.band_note_origin(doc, column.frame, m)
            };
            self.band_last = Some((y + m.last, m));
        }
        let Some(note) = self.notes.get(k).cloned() else { return };
        let label = doc.note_options(self.series).label(self.numbers.get(k).copied().unwrap_or(doc.note_options(self.series).start_at), true);
        self.placed.push(Placed { source: NoteSource::Story(k), note, label, part, fi: column.frame_index, col: column.column_index, m, text });
    }

    /// Continued text precedes references belonging to the new column's body.
    pub fn place_line(&mut self, doc: &Document, column: NoteColumn<'_>, extra: &[usize], plan: Option<SplitPlan>, opts: &ComposeOptions) -> bool {
        let before = self.placed.len();
        if let Some(c) = self.pending(column) {
            if plan.as_ref().is_some_and(|p| p.k == c.k) {
                if let Some(p) = plan {
                    self.place_split(doc, p, column);
                }
                return self.placed.len() != before;
            }
            if let Some((text, m)) = self.remainder(doc, &c, column, opts) {
                self.push(doc, c.k, c.heights.len(), text, m, column);
            }
            self.carry = None;
        }
        for &k in extra {
            if plan.as_ref().is_some_and(|p| p.k == k) {
                continue;
            }
            self.place(doc, k, column, opts);
        }
        if let Some(p) = plan {
            self.place_split(doc, p, column);
        }
        self.placed.len() != before
    }

    fn place_split(&mut self, doc: &Document, plan: SplitPlan, column: NoteColumn<'_>) {
        let part = plan.heights.len().saturating_sub(1);
        if let Some(span) = plan.band_span {
            self.band_spans.insert((plan.k, part), span);
            self.cache.clear();
        }
        self.push(doc, plan.k, part, plan.text, plan.m, column);
        self.carry = plan.remainder.then_some(Carry {
            k: plan.k,
            heights: plan.heights,
            pages: plan.pages,
            continuations: plan.continuations,
            exclusions: plan.exclusions,
            fi: column.frame_index,
            col: column.column_index,
            run_in: plan.run_in,
        });
    }

    /// Place a table cell's footnote at the foot of `column`.
    pub fn place_cell(&mut self, doc: &Document, n: &CellNote, column: NoteColumn<'_>, opts: &ComposeOptions) {
        if self.placed.iter().any(|p| p.source == n.source) {
            return;
        }
        let (text, m) = self.compose_note(doc, n.source, &n.label, &n.note, (column.width, column.frame), opts);
        self.placed.push(Placed {
            source: n.source,
            note: Arc::clone(&n.note),
            label: n.label.clone(),
            part: 0,
            fi: column.frame_index,
            col: column.column_index,
            m,
            text,
        });
    }

    /// Stack each column's footnotes against its bottom, rule above the first.
    pub fn finish(&mut self, doc: &Document, frames: &[FrameSpec], cols: &[Vec<Rect>], out: &mut ComposedStory) {
        if self.placed.is_empty() {
            return;
        }
        let o = doc.note_options(self.series);
        let mut groups: Vec<(usize, usize)> = self.placed.iter().map(|p| (p.fi, if self.span { 0 } else { p.col })).collect();
        groups.dedup();
        groups.sort_unstable();
        groups.dedup();
        for (fi, ci) in groups {
            let (Some(_), Some(col)) = (frames.get(fi), cols.get(fi).filter(|c| ci < c.len()).and_then(|c| self.area(c, ci))) else { continue };
            let here: Vec<&Placed> = self.placed.iter().filter(|p| self.same_area(p, fi, ci)).collect();
            let (count, width) = self.geometry(doc, col.width());
            let texts: Vec<_> = here.iter().map(|p| p.text.clone()).collect();
            let (total, fragments) = columns::balance(&texts, count, o.space_between);
            let total = if count == 1 { stack_height(here.iter().map(|p| &p.m), o.space_between) } else { total };
            let top = col.y1 - total;
            let Some(ft) = out.frames.get_mut(fi) else { continue };
            if o.rule.on && o.rule.weight > 0.0 {
                let x0 = col.x0 + o.rule.left_indent;
                let y = top + o.rule.offset - o.rule.weight;
                ft.decos.push(Deco {
                    rect: Rect::new(x0, y, (x0 + o.rule.width).min(col.x1), y + o.rule.weight),
                    color: o.rule.color.clone(),
                    tint: o.rule.tint,
                    footnote_rule: true,
                    ..Deco::default()
                });
            }
            if count > 1 {
                for fragment in fragments {
                    let Some(p) = here.get(fragment.note) else { continue };
                    let note = &p.note;
                    let physical =
                        if self.direction == designcraft_doc::TextDirection::RightToLeft { count - 1 - fragment.column } else { fragment.column };
                    let x = col.x0 + physical as f64 * (width + o.clamped_column_gutter());
                    ft.notes.push(PlacedNote {
                        id: note.id,
                        index: match p.source {
                            NoteSource::Story(k) | NoteSource::Cell { k, .. } => k,
                        },
                        series: self.series,
                        reference_baseline: 0.0,
                        overflow: false,
                        column: p.col as u32,
                        label: p.label.clone(),
                        origin: Point::new(x, top + fragment.y),
                        rect: Rect::new(x, top + fragment.top, x + width, top + fragment.bottom),
                        text: columns::fragment(&p.text, fragment.lines),
                        note: Arc::clone(note),
                        source: Arc::from(note.text.text.as_str()),
                    });
                }
                continue;
            }
            // Previous footnote's last baseline and bottom.
            let mut prev: Option<(f64, f64, NoteMetrics)> = None;
            for p in here {
                let (y, y0) = match prev {
                    None => (top + p.m.top_padding(), top),
                    Some((last, bottom, metrics)) => {
                        let first = last + o.space_between + metrics.advance_to(p.m);
                        (first - p.m.first, if p.m.lead <= 0.0 { first - p.m.ascent } else { (bottom + o.space_between).min(first) })
                    }
                };
                let y1 = y + p.m.last + p.m.descent;
                append_note(
                    &mut ft.notes,
                    PlacedNote {
                        id: p.note.id,
                        index: match p.source {
                            NoteSource::Story(k) | NoteSource::Cell { k, .. } => k,
                        },
                        column: p.col as u32,
                        series: self.series,
                        reference_baseline: 0.0,
                        overflow: false,
                        label: p.label.clone(),
                        origin: Point::new(col.x0, y),
                        rect: Rect::new(col.x0, y0, col.x1, y1),
                        text: p.text.clone(),
                        source: Arc::from(p.note.text.text.as_str()),
                        note: Arc::clone(&p.note),
                    },
                );
                prev = Some((y + p.m.last, y1, p.m));
            }
        }
    }
}

mod series;
pub(crate) use series::{NoteSnapshot, Notes, SplitPlan as SeriesSplitPlan};

/// The placed footnote with id `id`: (frame index, footnote).
pub fn find_note(cs: &ComposedStory, id: u64) -> Option<(usize, &PlacedNote)> {
    cs.frames.iter().enumerate().find_map(|(fi, ft)| ft.notes.iter().find(|n| n.id == id).map(|n| (fi, n)))
}

/// The footnote's text with every placed part in placement order.
/// Caret commands use it so line movement crosses parts like body frames;
/// each frame stays in its part's local coordinates.
pub fn note_text(cs: &ComposedStory, id: u64) -> Option<ComposedStory> {
    let mut parts = cs.frames.iter().flat_map(|ft| &ft.notes).filter(|n| n.id == id);
    let mut text = (*parts.next()?.text).clone();
    text.frames = text.frames.first().cloned().into_iter().chain(parts.filter_map(|n| n.text.frames.first().cloned())).collect();
    Some(text)
}

/// Find the footnote part holding byte `pos` for caret movement and selection drawing.
/// The story-end caret belongs to the last part.
pub fn find_note_part(cs: &ComposedStory, id: u64, pos: usize) -> Option<(usize, &PlacedNote)> {
    let mut last = None;
    for (fi, ft) in cs.frames.iter().enumerate() {
        for n in ft.notes.iter().filter(|n| n.id == id) {
            let range = &n.text.frames.first()?.range;
            if range.contains(&pos) {
                return Some((fi, n));
            }
            if pos == range.end {
                last = Some((fi, n));
            }
        }
    }
    last
}

/// Footnote text hit at frame-inner point `p`: (footnote id, byte in the footnote's story).
pub fn hit_note(cs: &ComposedStory, fi: usize, p: Point) -> Option<(u64, usize)> {
    hit_note_with(cs, fi, p, crate::hit)
}

/// [`hit_note`] with the hit test for the note's text (see [`crate::hit_cell_with`]).
pub fn hit_note_with(cs: &ComposedStory, fi: usize, p: Point, hit: fn(&ComposedStory, usize, Point) -> Option<usize>) -> Option<(u64, usize)> {
    let ft = cs.frames.get(fi)?;
    // Run-in fragments share a baseline and can have overlapping bounding rectangles.
    let n = ft.notes.iter().filter(|n| n.rect.contains(p)).min_by(|a, b| {
        let distance = |n: &PlacedNote| {
            n.text
                .frames
                .iter()
                .flat_map(|f| &f.lines)
                .flat_map(|l| {
                    l.glyphs.iter().map(move |g| {
                        let x = p.x - n.origin.x;
                        let y = p.y - n.origin.y;
                        let dx = (g.x - x).max(0.0).max(x - g.x - g.adv);
                        let dy = (l.baseline - l.ascent - y).max(0.0).max(y - l.baseline - l.descent);
                        dx * dx + dy * dy
                    })
                })
                .fold(f64::INFINITY, f64::min)
        };
        distance(a).total_cmp(&distance(b))
    })?;
    let local = Point::new(p.x - n.origin.x, p.y - n.origin.y);
    Some((n.id, hit(&n.text, 0, local).unwrap_or(0)))
}

/// Caret geometry for byte `pos` of a footnote's story, in frame inner space:
/// (frame index, x, baseline, ascent, descent).
pub fn note_caret(cs: &ComposedStory, id: u64, pos: usize) -> Option<(usize, f64, f64, f64, f64)> {
    let (fi, n) = find_note_part(cs, id, pos)?;
    match crate::caret(&n.text, pos) {
        Some((_, x, bl, a, d)) => Some((fi, x + n.origin.x, bl + n.origin.y, a, d)),
        None => Some((fi, n.origin.x, n.origin.y + 10.0, 9.0, 3.0)),
    }
}

mod page_margins;
#[cfg(test)]
mod run_in_tests;
pub(crate) use page_margins::stack_page_margins;

#[cfg(test)]
mod tests {
    use super::*;
    use designcraft_doc::{ParaFormat, SpreadRef, build::NewDocument, story};

    fn band_fixture(text: &str) -> (Document, designcraft_doc::StoryId, FrameSpec) {
        let mut doc = Document::new(&NewDocument::default());
        doc.footnote_options.first_baseline = designcraft_doc::FirstBaseline::Ascent;
        let (_, sid) =
            doc.add_text_frame(SpreadRef::Doc(0), Rect::new(0.0, 120.0, 300.0, 1000.0), doc.default_layer(), "Body", Default::default()).unwrap();
        doc.story_mut(sid).unwrap().insert_note(0, text, Default::default()).unwrap();
        let frame = crate::frame_specs(&doc, sid).remove(0);
        (doc, sid, frame)
    }

    #[test]
    fn band_origin_queries_do_not_walk_preceding_parts() {
        let (doc, sid, frame) = band_fixture("Note");
        let mut notes = SeriesNotes::new(&doc, doc.story(sid).unwrap(), 0, false);
        notes.band_column = Some((1, 0));
        notes.band_previous = Some((0.0, NoteMetrics::default()));
        for k in 0..100 {
            notes.push(
                &doc,
                k,
                0,
                Arc::new(ComposedStory::default()),
                NoteMetrics { first: 10.0, last: 10.0, lead: 12.0, descent: 2.0, ..Default::default() },
                NoteColumn { frame_index: 1, column_index: 0, width: 300.0, frame: &frame },
            );
        }
        ORIGIN_VISITS.with(|n| n.set(0));
        for _ in 0..128 {
            assert_eq!(notes.band_note_origin(&doc, &frame, NoteMetrics { first: 10.0, lead: 12.0, ..Default::default() }), 1202.0);
        }
        assert!(ORIGIN_VISITS.with(|n| n.get()) <= 128, "origin visits {}", ORIGIN_VISITS.with(|n| n.get()));
    }

    #[test]
    fn band_origin_is_rechecked_when_wrap_changes_first_line_leading() {
        let (mut doc, sid, mut frame) = band_fixture("aa aa LARGE trailing words");
        let text = &mut Arc::make_mut(&mut doc.story_mut(sid).unwrap().notes[0]).text;
        text.format_chars(0..text.len(), |f| {
            f.over.size = Some(12.0);
            f.over.leading = Some(designcraft_doc::Leading::Points(14.4));
        });
        let at = text.text.find("LARGE").unwrap();
        text.format_chars(at..at + 5, |f| {
            f.over.size = Some(30.0);
            f.over.leading = Some(designcraft_doc::Leading::Points(80.0));
            f.over.no_break = Some(true);
        });
        frame.exclusions.push(crate::Exclusion {
            rect: Rect::new(0.0, -100.0, 230.0, 120.0),
            mode: designcraft_doc::WrapMode::BoundingBox,
            side: designcraft_doc::WrapSide::BothSides,
            contour: None,
            after: None,
            anchor_baseline: None,
        });
        let mut notes = SeriesNotes::new(&doc, doc.story(sid).unwrap(), 0, false);
        notes.band_column = Some((1, 0));
        notes.band_previous = Some((30.0, NoteMetrics::default()));
        ORIGIN_LAST.with(|n| n.set(None));
        ORIGIN_RECHECKS.with(|n| n.set(0));
        let part = notes.compose(&doc, 0, 300.0, &frame, &ComposeOptions { note_bands: true, ..Default::default() });
        assert!((part.1.lead - 14.4).abs() < 0.01, "first leading {}", part.1.lead);
        let expected = notes.band_note_origin(&doc, &frame, part.1);
        assert!(
            (ORIGIN_LAST.with(|n| n.get()).unwrap() - expected).abs() < 0.01,
            "shifted origin {:?}, final origin {expected}",
            ORIGIN_LAST.with(|n| n.get())
        );
        assert!(ORIGIN_RECHECKS.with(|n| n.get()) <= 2);
    }

    #[test]
    fn note_part_stops_at_unplaceable_later_drop_word() {
        let mut doc = Document::new(&NewDocument::default());
        let (_, sid) =
            doc.add_text_frame(SpreadRef::Doc(0), Rect::new(0.0, 0.0, 80.0, 500.0), doc.default_layer(), "Body", ParaFormat::default()).unwrap();
        let prefix = "Fits.\n";
        let text = format!("{prefix}Extraordinary typography body");
        doc.story_mut(sid).unwrap().insert_note(0, &text, ParaFormat::default()).unwrap();
        let note = Arc::make_mut(&mut doc.story_mut(sid).unwrap().notes[0]);
        note.text.format_paras(prefix.len()..text.len(), |f| {
            f.para.drop_cap_words = Some(2);
            f.para.drop_cap_lines = Some(3);
        });
        let mut notes = SeriesNotes::new(&doc, doc.story(sid).unwrap(), 0, false, None);
        let frame = crate::frame_specs(&doc, sid).remove(0);
        let opts = ComposeOptions::default();
        let parts = notes.compose_parts(&doc, 0, 80.0, &frame, &opts, &[], &[]);
        let part = &parts[0].as_ref().unwrap().0;
        assert_eq!(part.overset_at, Some(prefix.len()));
        assert_eq!(part.frames[0].range.end, prefix.len());
        let parent = crate::compose_story(&doc, sid, &opts);
        assert!(parent.overset_at.is_some());
    }

    #[test]
    fn note_cache_distinguishes_parent_prefixes_in_current_and_earlier_parts() {
        let mut doc = Document::new(&NewDocument::default());
        let (_, sid) =
            doc.add_text_frame(SpreadRef::Doc(0), Rect::new(0.0, 0.0, 200.0, 100.0), doc.default_layer(), "Body", ParaFormat::default()).unwrap();
        let text = vec![story::PAGE_NUMBER.to_string(); 8].join("\n");
        doc.story_mut(sid).unwrap().insert_note(0, &text, ParaFormat::default()).unwrap();
        let mut notes = SeriesNotes::new(&doc, doc.story(sid).unwrap(), 0, false, None);
        let mut frame = crate::frame_specs(&doc, sid).remove(0);
        frame.page = None;
        frame.parent = true;
        frame.page_name = Some("A".into());
        let opts = ComposeOptions::default();
        let visible = |text: &ComposedStory| -> String {
            text.frames.iter().flat_map(|f| &f.lines).flat_map(|l| &l.glyphs).filter(|g| g.visible).map(|g| g.rendered_char).collect()
        };
        let first = notes.compose_parts(&doc, 0, 200.0, &frame, &opts, &[], &[]);
        assert!(visible(&first[0].as_ref().unwrap().0).contains('A'));
        let earlier_a = PartPage::of(&frame);
        frame.page_name = Some("B".into());
        let second = notes.compose_parts(&doc, 0, 200.0, &frame, &opts, &[], &[]);
        assert!(visible(&second[0].as_ref().unwrap().0).contains('B'), "parent B must not reuse parent A's markers");
        let earlier_b = PartPage::of(&frame);
        frame.page_name = Some("C".into());
        let heights = [(200.0, 30.0)];
        let first = notes.compose_parts(&doc, 0, 200.0, &frame, &opts, &heights, &[earlier_a]);
        assert!(visible(&first[0].as_ref().unwrap().0).contains('A'));
        let second = notes.compose_parts(&doc, 0, 200.0, &frame, &opts, &heights, &[earlier_b]);
        assert!(visible(&second[0].as_ref().unwrap().0).contains('B'), "earlier parent B must not reuse parent A's markers");
    }
}
