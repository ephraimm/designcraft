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
use crate::{ComposeOptions, ComposedStory, Deco, FrameSpec, PlacedNote};

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
mod streams;
pub use streams::{StreamPageFit, StreamPageInput, StreamState, fit_stream_page};

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
    /// First and last baselines (the first one includes the First Baseline offset).
    pub(super) first: f64,
    pub(super) last: f64,
    /// Leading of the first line: its baseline's distance below the previous footnote's last.
    pub(super) lead: f64,
    /// Descent of the last line.
    pub(super) descent: f64,
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
fn restart_key(doc: &Document, page: Option<usize>) -> Option<usize> {
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
    let mut h: Option<f64> = None;
    let mut descent = 0.0;
    for m in notes {
        let last = match h {
            None => m.last,
            Some(prev) => prev + space_between + m.lead + (m.last - m.first),
        };
        h = Some(last);
        descent = m.descent;
    }
    h.map_or(0.0, |h| h + descent)
}

/// Last placed part, including the (width, height) of each part's area used to thread its text.
#[derive(Clone)]
pub(crate) struct Carry {
    k: usize,
    heights: Vec<(f64, f64)>,
    fi: usize,
    col: usize,
}

pub(crate) struct SplitPlan {
    remainder: bool,
    k: usize,
    heights: Vec<(f64, f64)>,
    text: Arc<ComposedStory>,
    m: NoteMetrics,
}

type SeriesSnapshot = (u32, Option<usize>, usize, Option<Carry>);
type Part = (Arc<ComposedStory>, NoteMetrics);
type CacheKey = (NoteSource, String, u64, bool, bool, Option<usize>, Vec<(u64, u64)>);

/// The number a story's next footnote reference takes, and the restart unit it counts in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoteNumber {
    pub num: u32,
    /// The page, spread or section the number counts in (none when numbering never restarts).
    pub key: Option<usize>,
}

pub(super) struct SeriesNotes {
    series: u64,
    nested: bool,
    direction: designcraft_doc::TextDirection,
    members: Vec<bool>,
    /// Byte offsets of the reference characters (index = footnote index).
    refs: Vec<usize>,
    /// Number of each footnote (assigned when its paragraph is shaped).
    numbers: Vec<u32>,
    /// Next number and the restart unit (page / spread / section start) it counts in.
    pub num: u32,
    pub key: Option<usize>,
    pub placed: Vec<Placed>,
    /// Composed footnote text by (index, label, width).
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

    fn compose(&mut self, doc: &Document, k: usize, w: f64, f: &FrameSpec, opts: &ComposeOptions) -> Part {
        self.compose_parts(doc, k, w, f, opts, &[])
            .first()
            .and_then(Clone::clone)
            .unwrap_or_else(|| (Arc::new(ComposedStory::default()), NoteMetrics::default()))
    }

    /// One threaded composition lets the note's own paragraph keeps govern each frame break.
    /// `heights` holds the (width, height) of each earlier part; the last part is `w` wide and unbounded.
    fn compose_parts(&mut self, doc: &Document, k: usize, w: f64, f: &FrameSpec, opts: &ComposeOptions, heights: &[(f64, f64)]) -> Vec<Option<Part>> {
        let Some(note) = self.notes.get(k).cloned() else { return Vec::new() };
        let o = doc.note_options(self.series);
        let label = o.label(self.numbers.get(k).copied().unwrap_or(o.start_at), true);
        self.compose_note_parts(doc, NoteSource::Story(k), &label, &note, (w, f), opts, heights)
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
        self.compose_note_parts(doc, source, label, note, area, opts, &[])
            .first()
            .and_then(Clone::clone)
            .unwrap_or_else(|| (Arc::new(ComposedStory::default()), NoteMetrics::default()))
    }

    #[allow(clippy::too_many_arguments)]
    fn compose_note_parts(
        &mut self,
        doc: &Document,
        source: NoteSource,
        label: &str,
        note: &Footnote,
        (w, f): (f64, &FrameSpec),
        opts: &ComposeOptions,
        heights: &[(f64, f64)],
    ) -> Vec<Option<Part>> {
        let o = doc.note_options(self.series);
        let (_, w) = self.geometry(doc, w);
        let shown = format!("{label}{}", o.separator);
        // Page identity and earlier part areas affect variables and threaded note breaks.
        let key =
            (source, shown, w.to_bits(), f.left_page, f.right_page, f.page, heights.iter().map(|(pw, h)| (pw.to_bits(), h.to_bits())).collect());
        if let Some(c) = self.cache.get(&key) {
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
            label: Some(key.1.clone()),
            xrefs: opts.xrefs.clone(),
            placed: Vec::new(),
            notes_start: None,
        };
        let specs: Vec<FrameSpec> = heights
            .iter()
            .map(|&(pw, h)| FrameSpec { area: Rect::new(0.0, 0.0, pw.max(1.0), h), ..spec.clone() })
            .chain(std::iter::once(spec.clone()))
            .collect();
        let cs = crate::compose(doc, &note.text, &specs, &nopts);
        let parts = cs
            .frames
            .iter()
            .enumerate()
            .map(|(i, ft)| {
                let (Some(a), Some(z)) = (ft.lines.first(), ft.lines.last()) else { return None };
                let m = NoteMetrics { first: a.baseline, last: z.baseline, lead: a.leading, descent: z.descent };
                let mut frame = ft.clone();
                // Body frame ranges exclude paragraph separators; parts own those bytes too.
                frame.range.end = cs.frames.iter().skip(i.saturating_add(1)).find(|f| !f.lines.is_empty()).map_or(note.text.len(), |f| f.range.start);
                let text = ComposedStory { frames: vec![frame], ..cs.clone() };
                Some((Arc::new(text), m))
            })
            .collect::<Vec<_>>();
        self.cache.insert(key, parts.clone());
        parts
    }

    fn pending(&self, column: NoteColumn<'_>) -> Option<Carry> {
        self.carry
            .as_ref()
            .filter(|c| column.frame_index > c.fi || (column.frame_index == c.fi && !self.span && column.column_index > c.col))
            .cloned()
    }

    fn remainder(&mut self, doc: &Document, c: &Carry, column: NoteColumn<'_>, opts: &ComposeOptions) -> Option<Part> {
        self.compose_parts(doc, c.k, column.width, column.frame, opts, &c.heights).get(c.heights.len()).and_then(Clone::clone)
    }

    fn metrics(&mut self, doc: &Document, column: NoteColumn<'_>, extra: &[usize], opts: &ComposeOptions) -> Vec<NoteMetrics> {
        let mut ms: Vec<_> = self.placed.iter().filter(|p| self.same_area(p, column.frame_index, column.column_index)).map(|p| p.m).collect();
        if let Some(c) = self.pending(column)
            && let Some((_, m)) = self.remainder(doc, &c, column, opts)
        {
            ms.push(m);
        }
        for &k in extra {
            ms.push(self.compose(doc, k, column.width, column.frame, opts).1);
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
        let (k, mut heights, ms, original) = if let Some((&k, before)) = extra.split_last() {
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
            (k, Vec::new(), ms, self.compose(doc, k, column.width, column.frame, opts))
        } else {
            let c = pending?;
            let original = self.remainder(doc, &c, column, opts)?;
            let ms = self.placed.iter().filter(|p| self.same_area(p, column.frame_index, column.column_index)).map(|p| p.m).collect();
            (c.k, c.heights, ms, original)
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
        // Ordinary text frames fit baselines, but a footnote stack also includes descent.
        // Each retry drops its last line; the unsplit remainder bounds the attempts.
        let attempts = original.0.frames.first().map_or(0, |f| f.lines.len());
        for _ in 0..attempts {
            if !h.is_finite() || h <= 0.0 {
                return None;
            }
            *heights.get_mut(part)? = (width, h);
            let parts = self.compose_parts(doc, k, column.width, column.frame, opts, &heights);
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
                return Some(SplitPlan { remainder: parts.get(heights.len()).is_some_and(Option::is_some), k, heights, text, m });
            }
            let smaller = h.min(m.last - 0.02);
            if smaller >= h {
                return None;
            }
            h = smaller;
        }
        None
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
        let (space_between, space_before) = (o.space_between, o.space_before);
        let (count, _) = self.geometry(doc, column.width);
        let mut texts = self.area_texts(doc, column, own, opts);
        let mut ms = self.metrics(doc, column, own, opts);
        for n in cells {
            // A note placed already (here or in another column) counts once.
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
            (if count == 1 { stack_height(&ms, space_between) } else { columns::balance(&texts, count, space_between).0 }) + space_before
        }
    }

    pub fn place(&mut self, doc: &Document, k: usize, column: NoteColumn<'_>, opts: &ComposeOptions) {
        let (text, m) = self.compose(doc, k, column.width, column.frame, opts);
        self.push(doc, k, 0, text, m, column);
    }

    fn push(&mut self, doc: &Document, k: usize, part: usize, text: Arc<ComposedStory>, m: NoteMetrics, column: NoteColumn<'_>) {
        if self.placed.iter().any(|p| p.source == NoteSource::Story(k) && p.part == part) {
            return;
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
        self.push(doc, plan.k, part, plan.text, plan.m, column);
        self.carry = plan.remainder.then_some(Carry { k: plan.k, heights: plan.heights, fi: column.frame_index, col: column.column_index });
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
            let mut prev: Option<(f64, f64)> = None;
            for p in here {
                let (y, y0) = match prev {
                    None => (top, top),
                    Some((last, bottom)) => {
                        let first = last + o.space_between + p.m.lead;
                        (first - p.m.first, (bottom + o.space_between).min(first))
                    }
                };
                let y1 = y + p.m.last + p.m.descent;
                ft.notes.push(PlacedNote {
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
                });
                prev = Some((y + p.m.last, y1));
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
    let n = ft.notes.iter().find(|n| n.rect.contains(p))?;
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
pub(crate) use page_margins::stack_page_margins;
