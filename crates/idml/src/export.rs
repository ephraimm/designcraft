//! Document → IDML package.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use designcraft_color::{Color, ColorType, GradientKind, SwatchValue, swatch};
use designcraft_doc::{
    CharAttrs, CharFormat, ColumnsKind, Content, Document, Item, ListType, PageSide, ParaAttrs, ParaFormat, Rule, Shape, SpanColumns, Spread, Story,
    TextFrameOptions, story as st,
};
use designcraft_geom::{Affine, PathData};

use crate::base64_encode;
use crate::names::{self, CHAR_BUILTINS, OBJECT_BUILTINS, PARA_BUILTINS, escape_id, num, pt};
use crate::xml::{El, Node, document};

mod common;
mod designmap;
mod fonts_prefs;
mod graphics;
mod item;
mod spread;
mod story;
mod styles;
mod table;
mod text_attrs;

use common::pkg_root;
pub(crate) use common::uid;
use designmap::{CONTAINER, metadata_xmp};
pub(crate) use item::corner_names;
#[cfg(test)]
pub(crate) use item::path_to_uri;

const NO_PARA_ID: &str = "ParagraphStyle/$ID/[No paragraph style]";
const TOC_STYLE_ID: &str = "TOCStyle/Table of Contents";
const TOC_STYLE_NAME: &str = "Table of Contents";
const NO_CHAR_ID: &str = "CharacterStyle/$ID/[No character style]";

/// Export options.
#[derive(Clone, Debug)]
pub struct ExportOptions {
    /// Embed image data in the package (`<Contents>`). When false, images with a known link path
    /// are written as links only.
    pub embed_images: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions { embed_images: true }
    }
}

/// Export a document as an IDML package (ZIP bytes).
///
/// IDML has one sort key per index topic path. If native references disagree, the first
/// reference serialized for that path supplies its key, including an empty/default key.
/// Later references cannot overwrite it; no synthetic topic names or sort keys are created.
/// Consistent references retain their keys. Conflicting per-reference keys cannot all survive
/// this topic-level representation. This policy also applies to [`export_idml_with`].
pub fn export_idml(doc: &Document) -> Vec<u8> {
    export_idml_with(doc, &ExportOptions::default())
}

/// Export with options, using the index sort-key conflict policy of [`export_idml`].
pub fn export_idml_with(doc: &Document, opts: &ExportOptions) -> Vec<u8> {
    export_idml_report(doc, opts).0
}

/// [`export_idml_with`] and its warnings: an image written as a link only because the export's
/// embedded-image limit was reached is named in one, so the package is not silently incomplete.
pub fn export_idml_report(doc: &Document, opts: &ExportOptions) -> (Vec<u8>, Vec<String>) {
    export_with_limit(doc, opts, MAX_EMBEDDED_BYTES)
}

pub(crate) fn export_with_limit(doc: &Document, opts: &ExportOptions, embed_limit: usize) -> (Vec<u8>, Vec<String>) {
    let mut ex = Ex::new(doc, opts);
    ex.embed_limit = embed_limit;
    if any_footnote(doc, |n| n.series != 0 || matches!(doc.note_options(n.series).placement, designcraft_doc::NotePlacement::Margin { .. })) {
        ex.warnings.push(
            "IDML supports one footnote setup; additional note series and margin placement are exported as ordinary footnotes using series 0.".into(),
        );
    }
    if any_footnote(doc, |note| {
        let options = doc.note_options(note.series);
        matches!(options.placement, designcraft_doc::NotePlacement::Stream) || options.ref_hidden
    }) {
        ex.warnings
            .push("IDML cannot retain commentary stream routes or hidden note references; comments are exported as ordinary footnotes.".into());
    }
    }
    if any_footnote(doc, |note| doc.note_options(note.series).body_hidden) {
        ex.warnings.push("IDML cannot retain hidden note body labels; notes are exported with ordinary footnote numbers and separators.".into());
