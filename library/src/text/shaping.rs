use std::ops::Range;
use std::str::FromStr;

use az::SaturatingAs;
use rustybuzz::{Feature, Tag, UnicodeBuffer};
use typst::font::{Font, FontVariant};
use typst::util::SliceExt;
use unicode_script::{Script, UnicodeScript};

use super::*;
use crate::layout::SpanMapper;
use crate::prelude::*;

/// The result of shaping text.
///
/// This type contains owned or borrowed shaped text runs, which can be
/// measured, used to reshape substrings more quickly and converted into a
/// frame.
pub struct ShapedText<'a> {
    /// The start of the text in the full paragraph.
    pub base: usize,
    /// The text that was shaped.
    pub text: &'a str,
    /// The text direction.
    pub dir: Dir,
    /// The text's style properties.
    pub styles: StyleChain<'a>,
    /// The font variant.
    pub variant: FontVariant,
    /// The font size.
    pub size: Abs,
    /// The width of the text's bounding box.
    pub width: Abs,
    /// The shaped glyphs.
    pub glyphs: Cow<'a, [ShapedGlyph]>,
}

/// A single glyph resulting from shaping.
#[derive(Debug, Clone)]
pub struct ShapedGlyph {
    /// The font the glyph is contained in.
    pub font: Font,
    /// The glyph's index in the font.
    pub glyph_id: u16,
    /// The advance width of the glyph.
    pub x_advance: Em,
    /// The horizontal offset of the glyph.
    pub x_offset: Em,
    /// The vertical offset of the glyph.
    pub y_offset: Em,
    /// The byte range of this glyph's cluster in the full paragraph. A cluster
    /// is a sequence of one or multiple glyphs that cannot be separated and
    /// must always be treated as a union.
    pub range: Range<usize>,
    /// Whether splitting the shaping result before this glyph would yield the
    /// same results as shaping the parts to both sides of `text_index`
    /// separately.
    pub safe_to_break: bool,
    /// The first char in this glyph's cluster.
    pub c: char,
    /// The source code location of the glyph and its byte offset within it.
    pub span: (Span, u16),
}

#[derive(Debug, Clone, Default)]
pub struct Adjustability {
    /// The left and right strechability
    pub stretchability: (Em, Em),
    /// The left and right shrinkability
    pub shrinkability: (Em, Em),
}

impl ShapedGlyph {
    /// Whether the glyph is a space.
    pub fn is_space(&self) -> bool {
        matches!(self.c, ' ' | '\u{00A0}' | '　')
    }

    /// Whether the glyph is justifiable.
    pub fn is_justifiable(&self) -> bool {
        self.is_space()
            || self.is_cjk()
            || self.is_cjk_left_aligned_punctuation()
            || self.is_cjk_right_aligned_punctuation()
    }

    pub fn is_cjk(&self) -> bool {
        use Script::*;
        // U+30FC: Katakana-Hiragana Prolonged Sound Mark
        matches!(self.c.script(), Hiragana | Katakana | Han) || self.c == '\u{30FC}'
    }

    /// See https://www.w3.org/TR/clreq/#punctuation_width_adjustment
    pub fn is_cjk_left_aligned_punctuation(&self) -> bool {
        // CJK quotation marks shares codepoints with latin quotation marks.
        // But only the CJK ones have full width.
        if matches!(self.c, '”' | '’') && self.x_advance == Em::one() {
            return true;
        }

        matches!(self.c, '，' | '。' | '、' | '：' | '；' | '》' | '）' | '』' | '」')
    }

    /// See https://www.w3.org/TR/clreq/#punctuation_width_adjustment
    pub fn is_cjk_right_aligned_punctuation(&self) -> bool {
        // CJK quotation marks shares codepoints with latin quotation marks.
        // But only the CJK ones have full width.
        if matches!(self.c, '“' | '‘') && self.x_advance == Em::one() {
            return true;
        }

        matches!(self.c, '《' | '（' | '『' | '「')
    }

    pub fn adjustability(&self) -> Adjustability {
        let width = self.x_advance;
        if self.is_space() {
            Adjustability {
                // The number for spaces is from Knuth-Plass' paper
                stretchability: (Em::zero(), width / 2.0),
                shrinkability: (Em::zero(), width / 3.0),
            }
        } else if self.is_cjk_left_aligned_punctuation() {
            Adjustability {
                stretchability: (Em::zero(), Em::zero()),
                shrinkability: (Em::zero(), width / 2.0),
            }
        } else if self.is_cjk_right_aligned_punctuation() {
            Adjustability {
                stretchability: (Em::zero(), Em::zero()),
                shrinkability: (width / 2.0, Em::zero()),
            }
        } else {
            Adjustability::default()
        }
    }

    /// The stretchability of the character.
    pub fn stretchability(&self) -> (Em, Em) {
        self.adjustability().stretchability
    }

    /// The shrinkability of the character.
    pub fn shrinkability(&self) -> (Em, Em) {
        self.adjustability().shrinkability
    }
}

/// A side you can go toward.
enum Side {
    /// To the left-hand side.
    Left,
    /// To the right-hand side.
    Right,
}

impl<'a> ShapedText<'a> {
    /// Build the shaped text's frame.
    ///
    /// The `justification` defines how much extra advance width each
    /// [justifiable glyph](ShapedGlyph::is_justifiable) will get.
    pub fn build(
        &self,
        vt: &Vt,
        justification_ratio: f64,
        extra_justification: Abs,
    ) -> Frame {
        let (top, bottom) = self.measure(vt);
        let size = Size::new(self.width, top + bottom);

        let mut offset = Abs::zero();
        let mut frame = Frame::new(size);
        frame.set_baseline(top);

        let shift = TextElem::baseline_in(self.styles);
        let lang = TextElem::lang_in(self.styles);
        let decos = TextElem::deco_in(self.styles);
        let fill = TextElem::fill_in(self.styles);

        for ((font, y_offset), group) in
            self.glyphs.as_ref().group_by_key(|g| (g.font.clone(), g.y_offset))
        {
            let mut range = group[0].range.clone();
            for glyph in group {
                range.start = range.start.min(glyph.range.start);
                range.end = range.end.max(glyph.range.end);
            }

            let pos = Point::new(offset, top + shift - y_offset.at(self.size));
            let glyphs = group
                .iter()
                .map(|glyph| {
                    let adjustability_left = if justification_ratio < 0.0 {
                        glyph.shrinkability().0
                    } else {
                        glyph.stretchability().0
                    };
                    let adjustability_right = if justification_ratio < 0.0 {
                        glyph.shrinkability().1
                    } else {
                        glyph.stretchability().1
                    };

                    let justification_left = adjustability_left * justification_ratio;
                    let mut justification_right =
                        adjustability_right * justification_ratio;
                    if glyph.is_justifiable() {
                        justification_right +=
                            Em::from_length(extra_justification, self.size)
                    }

                    frame.size_mut().x += justification_left.at(self.size)
                        + justification_right.at(self.size);

                    Glyph {
                        id: glyph.glyph_id,
                        x_advance: glyph.x_advance
                            + justification_left
                            + justification_right,
                        x_offset: glyph.x_offset + justification_left,
                        range: (glyph.range.start - range.start).saturating_as()
                            ..(glyph.range.end - range.start).saturating_as(),
                        span: glyph.span,
                    }
                })
                .collect();

            let item = TextItem {
                font,
                size: self.size,
                lang,
                fill: fill.clone(),
                text: self.text[range.start - self.base..range.end - self.base].into(),
                glyphs,
            };

            let layer = frame.layer();
            let width = item.width();

            // Apply line decorations.
            for deco in &decos {
                decorate(&mut frame, deco, &item, shift, pos, width);
            }

            frame.insert(layer, pos, FrameItem::Text(item));
            offset += width;
        }

        // Apply metadata.
        frame.meta(self.styles, false);

        frame
    }

    /// Measure the top and bottom extent of this text.
    fn measure(&self, vt: &Vt) -> (Abs, Abs) {
        let mut top = Abs::zero();
        let mut bottom = Abs::zero();

        let top_edge = TextElem::top_edge_in(self.styles);
        let bottom_edge = TextElem::bottom_edge_in(self.styles);

        // Expand top and bottom by reading the font's vertical metrics.
        let mut expand = |font: &Font| {
            let metrics = font.metrics();
            top.set_max(top_edge.resolve(self.styles, metrics));
            bottom.set_max(-bottom_edge.resolve(self.styles, metrics));
        };

        if self.glyphs.is_empty() {
            // When there are no glyphs, we just use the vertical metrics of the
            // first available font.
            let world = vt.world;
            for family in families(self.styles) {
                if let Some(font) = world
                    .book()
                    .select(family.as_str(), self.variant)
                    .and_then(|id| world.font(id))
                {
                    expand(&font);
                    break;
                }
            }
        } else {
            for g in self.glyphs.iter() {
                expand(&g.font);
            }
        }

        (top, bottom)
    }

    /// How many glyphs are in the text where we can insert additional
    /// space when encountering underfull lines.
    pub fn justifiables(&self) -> usize {
        self.glyphs.iter().filter(|g| g.is_justifiable()).count()
    }

    /// Whether the last glyph is a CJK character which should not be justified
    /// on line end.
    pub fn cjk_justifiable_at_last(&self) -> bool {
        self.glyphs
            .last()
            .map(|g| g.is_cjk() || g.is_cjk_left_aligned_punctuation())
            .unwrap_or(false)
    }

    /// The stretchability of the text.
    pub fn stretchability(&self) -> Abs {
        self.glyphs
            .iter()
            .map(|g| g.stretchability().0 + g.stretchability().1)
            .sum::<Em>()
            .at(self.size)
    }

    /// The shrinkability of the text
    pub fn shrinkability(&self) -> Abs {
        self.glyphs
            .iter()
            .map(|g| g.shrinkability().0 + g.shrinkability().1)
            .sum::<Em>()
            .at(self.size)
    }

    /// Reshape a range of the shaped text, reusing information from this
    /// shaping process if possible.
    ///
    /// The text `range` is relative to the whole paragraph.
    pub fn reshape(
        &'a self,
        vt: &Vt,
        spans: &SpanMapper,
        text_range: Range<usize>,
    ) -> ShapedText<'a> {
        let text = &self.text[text_range.start - self.base..text_range.end - self.base];
        if let Some(glyphs) = self.slice_safe_to_break(text_range.clone()) {
            Self {
                base: text_range.start,
                text,
                dir: self.dir,
                styles: self.styles,
                size: self.size,
                variant: self.variant,
                width: glyphs.iter().map(|g| g.x_advance).sum::<Em>().at(self.size),
                glyphs: Cow::Borrowed(glyphs),
            }
        } else {
            shape(vt, text_range.start, text, spans, self.styles, self.dir)
        }
    }

    /// Push a hyphen to end of the text.
    pub fn push_hyphen(&mut self, vt: &Vt) {
        families(self.styles).find_map(|family| {
            let world = vt.world;
            let font = world
                .book()
                .select(family.as_str(), self.variant)
                .and_then(|id| world.font(id))?;
            let ttf = font.ttf();
            let glyph_id = ttf.glyph_index('-')?;
            let x_advance = font.to_em(ttf.glyph_hor_advance(glyph_id)?);
            let range = self
                .glyphs
                .last()
                .map(|g| g.range.end..g.range.end)
                .unwrap_or_default();
            self.width += x_advance.at(self.size);
            self.glyphs.to_mut().push(ShapedGlyph {
                font,
                glyph_id: glyph_id.0,
                x_advance,
                x_offset: Em::zero(),
                y_offset: Em::zero(),
                range,
                safe_to_break: true,
                c: '-',
                span: (Span::detached(), 0),
            });
            Some(())
        });
    }

    /// Find the subslice of glyphs that represent the given text range if both
    /// sides are safe to break.
    fn slice_safe_to_break(&self, text_range: Range<usize>) -> Option<&[ShapedGlyph]> {
        let Range { mut start, mut end } = text_range;
        if !self.dir.is_positive() {
            std::mem::swap(&mut start, &mut end);
        }

        let left = self.find_safe_to_break(start, Side::Left)?;
        let right = self.find_safe_to_break(end, Side::Right)?;
        Some(&self.glyphs[left..right])
    }

    /// Find the glyph offset matching the text index that is most towards the
    /// given side and safe-to-break.
    fn find_safe_to_break(&self, text_index: usize, towards: Side) -> Option<usize> {
        let ltr = self.dir.is_positive();

        // Handle edge cases.
        let len = self.glyphs.len();
        if text_index == self.base {
            return Some(if ltr { 0 } else { len });
        } else if text_index == self.base + self.text.len() {
            return Some(if ltr { len } else { 0 });
        }

        // Find any glyph with the text index.
        let mut idx = self
            .glyphs
            .binary_search_by(|g| {
                let ordering = g.range.start.cmp(&text_index);
                if ltr {
                    ordering
                } else {
                    ordering.reverse()
                }
            })
            .ok()?;

        let next = match towards {
            Side::Left => usize::checked_sub,
            Side::Right => usize::checked_add,
        };

        // Search for the outermost glyph with the text index.
        while let Some(next) = next(idx, 1) {
            if self.glyphs.get(next).map_or(true, |g| g.range.start != text_index) {
                break;
            }
            idx = next;
        }

        // RTL needs offset one because the left side of the range should be
        // exclusive and the right side inclusive, contrary to the normal
        // behaviour of ranges.
        self.glyphs[idx].safe_to_break.then_some(idx + usize::from(!ltr))
    }
}

impl Debug for ShapedText<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        self.text.fmt(f)
    }
}

/// Holds shaping results and metadata common to all shaped segments.
struct ShapingContext<'a, 'v> {
    vt: &'a Vt<'v>,
    spans: &'a SpanMapper,
    glyphs: Vec<ShapedGlyph>,
    used: Vec<Font>,
    styles: StyleChain<'a>,
    size: Abs,
    variant: FontVariant,
    tags: Vec<rustybuzz::Feature>,
    fallback: bool,
    dir: Dir,
}

/// Shape text into [`ShapedText`].
pub fn shape<'a>(
    vt: &Vt,
    base: usize,
    text: &'a str,
    spans: &SpanMapper,
    styles: StyleChain<'a>,
    dir: Dir,
) -> ShapedText<'a> {
    let size = TextElem::size_in(styles);
    let mut ctx = ShapingContext {
        vt,
        spans,
        size,
        glyphs: vec![],
        used: vec![],
        styles,
        variant: variant(styles),
        tags: tags(styles),
        fallback: TextElem::fallback_in(styles),
        dir,
    };

    if !text.is_empty() {
        shape_segment(&mut ctx, base, text, families(styles));
    }

    track_and_space(&mut ctx);

    ShapedText {
        base,
        text,
        dir,
        styles,
        variant: ctx.variant,
        size,
        width: ctx.glyphs.iter().map(|g| g.x_advance).sum::<Em>().at(size),
        glyphs: Cow::Owned(ctx.glyphs),
    }
}

/// Shape text with font fallback using the `families` iterator.
fn shape_segment(
    ctx: &mut ShapingContext,
    base: usize,
    text: &str,
    mut families: impl Iterator<Item = FontFamily> + Clone,
) {
    // Fonts dont have newlines and tabs.
    if text.chars().all(|c| c == '\n' || c == '\t') {
        return;
    }

    // Find the next available family.
    let world = ctx.vt.world;
    let book = world.book();
    let mut selection = families.find_map(|family| {
        book.select(family.as_str(), ctx.variant)
            .and_then(|id| world.font(id))
            .filter(|font| !ctx.used.contains(font))
    });

    // Do font fallback if the families are exhausted and fallback is enabled.
    if selection.is_none() && ctx.fallback {
        let first = ctx.used.first().map(Font::info);
        selection = book
            .select_fallback(first, ctx.variant, text)
            .and_then(|id| world.font(id))
            .filter(|font| !ctx.used.contains(font));
    }

    // Extract the font id or shape notdef glyphs if we couldn't find any font.
    let Some(font) = selection else {
        if let Some(font) = ctx.used.first().cloned() {
            shape_tofus(ctx, base, text, font);
        }
        return;
    };

    ctx.used.push(font.clone());

    // Fill the buffer with our text.
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_language(language(ctx.styles));
    buffer.set_direction(match ctx.dir {
        Dir::LTR => rustybuzz::Direction::LeftToRight,
        Dir::RTL => rustybuzz::Direction::RightToLeft,
        _ => unimplemented!("vertical text layout"),
    });

    // Shape!
    let buffer = rustybuzz::shape(font.rusty(), &ctx.tags, buffer);
    let infos = buffer.glyph_infos();
    let pos = buffer.glyph_positions();
    let ltr = ctx.dir.is_positive();

    // Collect the shaped glyphs, doing fallback and shaping parts again with
    // the next font if necessary.
    let mut i = 0;
    while i < infos.len() {
        let info = &infos[i];
        let cluster = info.cluster as usize;

        // Add the glyph to the shaped output.
        if info.glyph_id != 0 {
            // Determine the text range of the glyph.
            let start = base + cluster;
            let end = base
                + if ltr { i.checked_add(1) } else { i.checked_sub(1) }
                    .and_then(|last| infos.get(last))
                    .map_or(text.len(), |info| info.cluster as usize);

            ctx.glyphs.push(ShapedGlyph {
                font: font.clone(),
                glyph_id: info.glyph_id as u16,
                // TODO: Don't ignore y_advance.
                x_advance: font.to_em(pos[i].x_advance),
                x_offset: font.to_em(pos[i].x_offset),
                y_offset: font.to_em(pos[i].y_offset),
                range: start..end,
                safe_to_break: !info.unsafe_to_break(),
                c: text[cluster..].chars().next().unwrap(),
                span: ctx.spans.span_at(start),
            });
        } else {
            // First, search for the end of the tofu sequence.
            let k = i;
            while infos.get(i + 1).map_or(false, |info| info.glyph_id == 0) {
                i += 1;
            }

            // Then, determine the start and end text index for the tofu
            // sequence.
            //
            // Examples:
            // Everything is shown in visual order. Tofus are written as "_".
            // We want to find out that the tofus span the text `2..6`.
            // Note that the clusters are longer than 1 char.
            //
            // Left-to-right:
            // Text:     h a l i h a l l o
            // Glyphs:   A   _   _   C   E
            // Clusters: 0   2   4   6   8
            //              k=1 i=2
            //
            // Right-to-left:
            // Text:     O L L A H I L A H
            // Glyphs:   E   C   _   _   A
            // Clusters: 8   6   4   2   0
            //                  k=2 i=3
            let start = infos[if ltr { k } else { i }].cluster as usize;
            let end = if ltr { i.checked_add(1) } else { k.checked_sub(1) }
                .and_then(|last| infos.get(last))
                .map_or(text.len(), |info| info.cluster as usize);

            // Trim half-baked cluster.
            let remove = base + start..base + end;
            while ctx.glyphs.last().map_or(false, |g| remove.contains(&g.range.start)) {
                ctx.glyphs.pop();
            }

            // Recursively shape the tofu sequence with the next family.
            shape_segment(ctx, base + start, &text[start..end], families.clone());
        }

        i += 1;
    }

    ctx.used.pop();
}

/// Shape the text with tofus from the given font.
fn shape_tofus(ctx: &mut ShapingContext, base: usize, text: &str, font: Font) {
    let x_advance = font.advance(0).unwrap_or_default();
    for (cluster, c) in text.char_indices() {
        let start = base + cluster;
        let end = start + c.len_utf8();
        ctx.glyphs.push(ShapedGlyph {
            font: font.clone(),
            glyph_id: 0,
            x_advance,
            x_offset: Em::zero(),
            y_offset: Em::zero(),
            range: start..end,
            safe_to_break: true,
            c,
            span: ctx.spans.span_at(start),
        });
    }
}

/// Apply tracking and spacing to the shaped glyphs.
fn track_and_space(ctx: &mut ShapingContext) {
    let tracking = Em::from_length(TextElem::tracking_in(ctx.styles), ctx.size);
    let spacing =
        TextElem::spacing_in(ctx.styles).map(|abs| Em::from_length(abs, ctx.size));

    let mut glyphs = ctx.glyphs.iter_mut().peekable();
    while let Some(glyph) = glyphs.next() {
        // Make non-breaking space same width as normal space.
        if glyph.c == '\u{00A0}' {
            glyph.x_advance -= nbsp_delta(&glyph.font).unwrap_or_default();
        }

        if glyph.is_space() {
            glyph.x_advance = spacing.relative_to(glyph.x_advance);
        }

        if glyphs
            .peek()
            .map_or(false, |next| glyph.range.start != next.range.start)
        {
            glyph.x_advance += tracking;
        }
    }
}

/// Difference between non-breaking and normal space.
fn nbsp_delta(font: &Font) -> Option<Em> {
    let space = font.ttf().glyph_index(' ')?.0;
    let nbsp = font.ttf().glyph_index('\u{00A0}')?.0;
    Some(font.advance(nbsp)? - font.advance(space)?)
}

/// Resolve the font variant.
pub fn variant(styles: StyleChain) -> FontVariant {
    let mut variant = FontVariant::new(
        TextElem::style_in(styles),
        TextElem::weight_in(styles),
        TextElem::stretch_in(styles),
    );

    let delta = TextElem::delta_in(styles);
    variant.weight = variant
        .weight
        .thicken(delta.clamp(i16::MIN as i64, i16::MAX as i64) as i16);

    if TextElem::emph_in(styles) {
        variant.style = match variant.style {
            FontStyle::Normal => FontStyle::Italic,
            FontStyle::Italic => FontStyle::Normal,
            FontStyle::Oblique => FontStyle::Normal,
        }
    }

    variant
}

/// Resolve a prioritized iterator over the font families.
pub fn families(styles: StyleChain) -> impl Iterator<Item = FontFamily> + Clone {
    const FALLBACKS: &[&str] = &[
        "linux libertine",
        "twitter color emoji",
        "noto color emoji",
        "apple color emoji",
        "segoe ui emoji",
    ];

    let tail = if TextElem::fallback_in(styles) { FALLBACKS } else { &[] };
    TextElem::font_in(styles)
        .into_iter()
        .chain(tail.iter().copied().map(FontFamily::new))
}

/// Collect the tags of the OpenType features to apply.
fn tags(styles: StyleChain) -> Vec<Feature> {
    let mut tags = vec![];
    let mut feat = |tag, value| {
        tags.push(Feature::new(Tag::from_bytes(tag), value, ..));
    };

    // Features that are on by default in Harfbuzz are only added if disabled.
    if !TextElem::kerning_in(styles) {
        feat(b"kern", 0);
    }

    // Features that are off by default in Harfbuzz are only added if enabled.
    if TextElem::smallcaps_in(styles) {
        feat(b"smcp", 1);
    }

    if TextElem::alternates_in(styles) {
        feat(b"salt", 1);
    }

    let storage;
    if let Some(set) = TextElem::stylistic_set_in(styles) {
        storage = [b's', b's', b'0' + set.get() / 10, b'0' + set.get() % 10];
        feat(&storage, 1);
    }

    if !TextElem::ligatures_in(styles) {
        feat(b"liga", 0);
        feat(b"clig", 0);
    }

    if TextElem::discretionary_ligatures_in(styles) {
        feat(b"dlig", 1);
    }

    if TextElem::historical_ligatures_in(styles) {
        feat(b"hilg", 1);
    }

    match TextElem::number_type_in(styles) {
        Smart::Auto => {}
        Smart::Custom(NumberType::Lining) => feat(b"lnum", 1),
        Smart::Custom(NumberType::OldStyle) => feat(b"onum", 1),
    }

    match TextElem::number_width_in(styles) {
        Smart::Auto => {}
        Smart::Custom(NumberWidth::Proportional) => feat(b"pnum", 1),
        Smart::Custom(NumberWidth::Tabular) => feat(b"tnum", 1),
    }

    if TextElem::slashed_zero_in(styles) {
        feat(b"zero", 1);
    }

    if TextElem::fractions_in(styles) {
        feat(b"frac", 1);
    }

    for (tag, value) in TextElem::features_in(styles).0 {
        tags.push(Feature::new(tag, value, ..))
    }

    tags
}

/// Process the language and and region of a style chain into a
/// rustybuzz-compatible BCP 47 language.
fn language(styles: StyleChain) -> rustybuzz::Language {
    let mut bcp: EcoString = TextElem::lang_in(styles).as_str().into();
    if let Some(region) = TextElem::region_in(styles) {
        bcp.push('-');
        bcp.push_str(region.as_str());
    }
    rustybuzz::Language::from_str(&bcp).unwrap()
}
