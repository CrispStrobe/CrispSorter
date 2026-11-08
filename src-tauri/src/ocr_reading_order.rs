//! Conservative reading order for reviewed OCR lines and semantic layout blocks.
//!
//! This is a geometric, left-to-right heuristic. It keeps the native order inside
//! each semantic block, visits separated columns in sequence, and places spanning
//! headings/footers between their surrounding column bands. It does not infer
//! typography from overlapping boxes or claim to solve every document layout.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn right(self) -> f32 { self.x + self.w }
    fn bottom(self) -> f32 { self.y + self.h }
    fn area(self) -> f32 { self.w * self.h }
    fn valid(self) -> bool {
        [self.x, self.y, self.w, self.h, self.right(), self.bottom(), self.area()]
            .iter().all(|n| n.is_finite()) && self.w > 0.0 && self.h > 0.0
    }
    fn intersection(self, other: Self) -> f32 {
        (self.right().min(other.right()) - self.x.max(other.x)).max(0.0)
            * (self.bottom().min(other.bottom()) - self.y.max(other.y)).max(0.0)
    }
    fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self { x, y, w: self.right().max(other.right()) - x, h: self.bottom().max(other.bottom()) - y }
    }
}

struct Block {
    rect: Rect,
    members: Vec<usize>,
}

fn bounds(blocks: &[Block], indices: &[usize]) -> Rect {
    indices.iter().skip(1).fold(blocks[indices[0]].rect, |rect, &i| rect.union(blocks[i].rect))
}

fn stable_blocks(blocks: &[Block], indices: &[usize]) -> Vec<usize> {
    let mut result = indices.to_vec();
    result.sort_by_key(|&i| blocks[i].members[0]);
    result
}

/// Return a complete permutation of region indices. Invalid geometry, absent
/// layout evidence, excessive fragmentation, or ambiguous overlap preserves the
/// existing order. A region is assigned to at most one layout block; unmatched
/// text is always retained. Layout boxes themselves never create or delete text.
pub(crate) fn order_regions(regions: &[Rect], layout: &[Rect]) -> Vec<usize> {
    let original = || (0..regions.len()).collect::<Vec<_>>();
    if regions.len() < 2 || layout.is_empty() || layout.len() > 4096
        || regions.iter().any(|r| !r.valid()) {
        return original();
    }
    let mut groups = HashMap::new();
    let mut blocks: Vec<Block> = Vec::new();
    for (index, &rect) in regions.iter().enumerate() {
        // The smallest layout box containing most of a region is more specific
        // than a page-sized or enclosing section box. Never split an OCR line.
        let containing = layout.iter().enumerate()
            .filter(|(_, candidate)| candidate.valid() && rect.intersection(**candidate) / rect.area() >= 0.75)
            .min_by(|(a, left), (b, right)| left.area().total_cmp(&right.area()).then(a.cmp(b)))
            .map(|(i, _)| i);
        let key = containing.unwrap_or(layout.len() + index);
        if let Some(&block_index) = groups.get(&key) {
            let block: &mut Block = &mut blocks[block_index];
            block.rect = block.rect.union(rect);
            block.members.push(index);
        } else {
            groups.insert(key, blocks.len());
            blocks.push(Block { rect, members: vec![index] });
        }
    }
    // Bound the pairwise ambiguity check even for noisy, fragmented scans.
    if blocks.len() > 2048 { return original(); }
    let indices: Vec<_> = (0..blocks.len()).collect();
    match order_blocks(&blocks, &indices, 0) {
        Ok(order) => order.into_iter().flat_map(|i| blocks[i].members.iter().copied()).collect(),
        Err(()) => original(),
    }
}

fn order_blocks(blocks: &[Block], indices: &[usize], depth: usize) -> Result<Vec<usize>, ()> {
    if indices.len() < 2 { return Ok(indices.to_vec()); }
    if depth > 32 { return Err(()); }
    for (position, &i) in indices.iter().enumerate() {
        for &j in &indices[position + 1..] {
            let a = blocks[i].rect;
            let b = blocks[j].rect;
            if a.intersection(b) > 0.25 * a.area().min(b.area()) { return Err(()); }
        }
    }
    let page = bounds(blocks, indices);
    let mut intervals: Vec<_> = indices.iter().map(|&i| blocks[i].rect)
        .filter(|r| r.w <= page.w * 0.65).map(|r| (r.x, r.right())).collect();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let Some(&(first, mut end)) = intervals.first() else { return Ok(stable_blocks(blocks, indices)); };
    let mut gap = (first, first);
    for &(start, right) in intervals.iter().skip(1) {
        if start - end > gap.1 - gap.0 { gap = (end, start); }
        end = end.max(right);
    }
    // Ordinary word spacing is not enough evidence for a column boundary.
    if gap.1 - gap.0 < 12.0_f32.max(page.w * 0.035) {
        return Ok(stable_blocks(blocks, indices));
    }
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut spanning = Vec::new();
    for &i in indices {
        let r = blocks[i].rect;
        if r.right() <= gap.0 + 0.01 { left.push(i); }
        else if r.x >= gap.1 - 0.01 { right.push(i); }
        else { spanning.push(i); }
    }
    if left.is_empty() || right.is_empty() { return Ok(stable_blocks(blocks, indices)); }
    let a = bounds(blocks, &left);
    let b = bounds(blocks, &right);
    if a.bottom().min(b.bottom()) <= a.y.max(b.y) {
        return Ok(stable_blocks(blocks, indices));
    }
    if spanning.is_empty() {
        let mut result = order_blocks(blocks, &left, depth + 1)?;
        result.extend(order_blocks(blocks, &right, depth + 1)?);
        return Ok(result);
    }
    spanning.sort_by(|&a, &b| blocks[a].rect.y.total_cmp(&blocks[b].rect.y)
        .then(blocks[a].members[0].cmp(&blocks[b].members[0])));
    let mut pending = left;
    pending.extend(right);
    let mut result = Vec::new();
    let mut previous_bottom = f32::NEG_INFINITY;
    for i in spanning {
        let separator = blocks[i].rect;
        if separator.y < previous_bottom - 0.01 { return Err(()); }
        let mut before = Vec::new();
        let mut after = Vec::new();
        for j in pending {
            let r = blocks[j].rect;
            if r.bottom() <= separator.y + 0.01 { before.push(j); }
            else if r.y >= separator.bottom() - 0.01 { after.push(j); }
            else { return Err(()); }
        }
        result.extend(order_blocks(blocks, &before, depth + 1)?);
        result.push(i);
        pending = after;
        previous_bottom = separator.bottom();
    }
    result.extend(order_blocks(blocks, &pending, depth + 1)?);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect { Rect { x, y, w, h } }

    #[test]
    fn two_columns_follow_each_column_before_the_next() {
        let boxes = [rect(0.0, 0.0, 80.0, 15.0), rect(120.0, 0.0, 80.0, 15.0),
            rect(0.0, 25.0, 80.0, 15.0), rect(120.0, 25.0, 80.0, 15.0)];
        assert_eq!(order_regions(&boxes, &boxes), vec![0, 2, 1, 3]);
    }

    #[test]
    fn spanning_heading_and_footer_surround_column_content() {
        let boxes = [rect(0.0, 0.0, 220.0, 15.0), rect(0.0, 30.0, 90.0, 15.0),
            rect(130.0, 30.0, 90.0, 15.0), rect(0.0, 60.0, 90.0, 15.0),
            rect(130.0, 60.0, 90.0, 15.0), rect(0.0, 100.0, 220.0, 15.0)];
        assert_eq!(order_regions(&boxes, &boxes), vec![0, 1, 3, 2, 4, 5]);
    }

    #[test]
    fn full_width_section_break_separates_column_bands() {
        let boxes = [rect(0.0, 0.0, 80.0, 15.0), rect(120.0, 0.0, 80.0, 15.0),
            rect(0.0, 30.0, 200.0, 15.0), rect(0.0, 60.0, 80.0, 15.0),
            rect(120.0, 60.0, 80.0, 15.0), rect(0.0, 90.0, 80.0, 15.0),
            rect(120.0, 90.0, 80.0, 15.0)];
        assert_eq!(order_regions(&boxes, &boxes), vec![0, 1, 2, 3, 5, 4, 6]);
    }

    #[test]
    fn semantic_rows_keep_their_native_word_order() {
        let regions = [rect(0.0, 0.0, 30.0, 12.0), rect(40.0, 0.0, 30.0, 12.0),
            rect(120.0, 0.0, 70.0, 12.0), rect(0.0, 20.0, 70.0, 12.0),
            rect(120.0, 20.0, 70.0, 12.0)];
        let layout = [rect(0.0, 0.0, 75.0, 40.0), rect(120.0, 0.0, 75.0, 40.0)];
        assert_eq!(order_regions(&regions, &layout), vec![0, 1, 3, 2, 4]);
    }

    #[test]
    fn ambiguous_overlap_preserves_the_exact_existing_order() {
        let boxes = [rect(180.0, 0.0, 60.0, 40.0), rect(50.0, 10.0, 100.0, 40.0),
            rect(0.0, 0.0, 100.0, 40.0)];
        assert_eq!(order_regions(&boxes, &boxes), vec![0, 1, 2]);
    }

    #[test]
    fn unmatched_text_and_enclosing_layout_never_drop_or_duplicate_regions() {
        let boxes = [rect(0.0, 0.0, 80.0, 12.0), rect(120.0, 0.0, 80.0, 12.0),
            rect(0.0, 20.0, 80.0, 12.0), rect(120.0, 20.0, 80.0, 12.0),
            rect(0.0, 50.0, 200.0, 12.0)];
        let layout = [rect(0.0, 0.0, 200.0, 40.0), rect(0.0, 0.0, 80.0, 40.0),
            rect(120.0, 0.0, 80.0, 40.0)];
        let result = order_regions(&boxes, &layout);
        assert_eq!(result, vec![0, 2, 1, 3, 4]);
        let mut all = result;
        all.sort_unstable();
        assert_eq!(all, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn missing_layout_or_invalid_regions_keep_native_order() {
        let boxes = [rect(100.0, 0.0, 40.0, 10.0), rect(0.0, 0.0, 40.0, 10.0)];
        assert_eq!(order_regions(&boxes, &[]), vec![0, 1]);
        let broken = [boxes[0], rect(f32::NAN, 0.0, 40.0, 10.0)];
        assert_eq!(order_regions(&broken, &boxes), vec![0, 1]);
    }
}
