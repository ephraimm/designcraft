# Run-in notes

`footnote.options` and `footnote.series.options` accept `runIn` (default false)
and `runInSeparator` (default one space, at most 64 Unicode characters).
The Document Footnote Options layout tab exposes both fields. Control and MCP
use the same engine commands.

`footnote.insert {text, series?, runIn?}` sets an optional per-note override.
`footnote.setRunIn {story, ids, runIn}` changes existing notes; `null` restores
the series setting. `footnote.list` reports the override. These edits support undo.
Native files retain the settings; older documents keep separate paragraphs.

In column feet, stream-frame columns and margins, the next note can continue
on the preceding note's last line. The separator precedes its number and text.
Space Between Footnotes does not apply to a run-in note, including when its
first word needs the next line. First-line indent, space before and drop caps
apply at paragraph starts. Further paragraphs keep their own formats.
With `bodyHidden`, the run-in separator remains while the note number and its
separator are hidden, in both column feet and stream frames.
Split notes retain source byte ranges and replay the first part's line position
and page context when fitting subsequent areas.

The first note in each area starts a paragraph. An explicit `runIn:false` also
starts a paragraph. Margin notes start a paragraph when running in would put
their first line above the reference baseline. Different margin hosts remain
separate areas; the document-wide margin pass displaces a joined group together.
Explicit paragraph keeps and required column/frame/page starts also take precedence
and start a paragraph. Table anchors retain their separate paragraphs and table ids.
An unbreakable first word moves to the next line when the preceding line has
insufficient room. Anchor ordering, same-page starts and overflow reporting
continue to apply independently to every note.
