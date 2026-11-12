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

/// Translate a real crop-local recognition box back to its source page without
/// changing its extent. Out-of-crop results are errors, never clamped repairs.
pub(crate) fn crop_rect_to_page(local: Rect, crop: Rect, width: u32, height: u32) -> Option<Rect> {
    if !local.valid() || !crop.valid() || local.x < 0.0 || local.y < 0.0
        || crop.x < 0.0 || crop.y < 0.0 || crop.right() > width as f32
        || crop.bottom() > height as f32 || local.right() > crop.w + 0.01
        || local.bottom() > crop.h + 0.01 {
        return None;
    }
    Some(Rect { x: local.x + crop.x, y: local.y + crop.y, ..local })
}

/// Plan bounded, nonoverlapping recognition inputs only when CC geometry proves
/// multiple columns. Otherwise retain the existing whole-page recognition path.
/// Adjacent components of the same printed line (including spanning headings)
/// are coalesced before finding gutters; no recognized text is involved.
pub(crate) fn column_recognition_crops(layout: &[Rect], width: u32, height: u32) -> Option<Vec<Rect>> {
    if layout.len() < 4 || layout.len() > 64 || width == 0 || height == 0
        || layout.iter().any(|r| !r.valid() || r.x < 0.0 || r.y < 0.0
            || r.right() > width as f32 || r.bottom() > height as f32) {
        return None;
    }
    let mut raw = layout.to_vec();
    raw.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
    let mut lines: Vec<Rect> = Vec::new();
    for r in raw {
        if let Some(line) = lines.iter_mut().rev().take(4).find(|line| {
            let overlap = line.bottom().min(r.bottom()) - line.y.max(r.y);
            overlap >= 0.5 * line.h.min(r.h) && r.x >= line.x
                && r.x - line.right() <= 2.0 * line.h.min(r.h)
        }) {
            *line = line.union(r);
        } else {
            lines.push(r);
        }
    }
    lines.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
    let order = order_regions(&lines, &lines);
    if order.iter().copied().eq(0..lines.len()) { return None; }
    let crops: Vec<_> = order.into_iter().map(|i| {
        let r = lines[i];
        let pad = (r.h * 0.25).clamp(4.0, 20.0);
        let x = (r.x - pad).floor().max(0.0);
        let y = (r.y - pad).floor().max(0.0);
        let right = (r.right() + pad).ceil().min(width as f32);
        let bottom = (r.bottom() + pad).ceil().min(height as f32);
        Rect { x, y, w: right - x, h: bottom - y }
    }).collect();
    // Never recognize the same pixels in two crops, or amplify page-sized
    // inference into an unbounded crop batch. Ambiguity keeps native behavior.
    if crops.iter().map(|r| r.area() as f64).sum::<f64>() > f64::from(width) * f64::from(height)
        || crops.iter().enumerate().any(|(i, a)| crops[i + 1..].iter().any(|b| a.intersection(*b) > 0.0)) {
        return None;
    }
    Some(crops)
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

    #[test]
    fn retained_android_cc_geometry_plans_heading_then_complete_columns() {
        // Real CC boxes from Android run 43275466921, not recognized text.
        // CC split the heading at a word gap; DBNet joined the body columns.
        let boxes = [rect(300.0,74.0,860.0,54.0), rect(1197.0,74.0,297.0,54.0),
            rect(81.0,260.0,377.0,38.0),rect(1043.0,260.0,368.0,38.0),
            rect(85.0,409.0,514.0,48.0),rect(1043.0,409.0,472.0,48.0),
            rect(80.0,559.0,554.0,39.0),rect(1045.0,559.0,491.0,48.0),
            rect(80.0,709.0,424.0,39.0),rect(1045.0,709.0,532.0,39.0)];
        let crops = column_recognition_crops(&boxes, 1800, 1000).unwrap();
        assert_eq!(crops.len(), 9);
        assert!(crops[0].x < 300.0 && crops[0].right() > 1494.0);
        assert!(crops[1..5].iter().all(|r| r.right() < 810.0));
        assert!(crops[5..].iter().all(|r| r.x > 990.0));
        for column in [&crops[1..5], &crops[5..]] {
            assert!(column.windows(2).all(|pair| pair[0].y < pair[1].y));
        }
        // Every detected pixel belongs to exactly one padded recognition crop.
        for b in boxes {
            assert_eq!(crops.iter().filter(|c| c.intersection(b) >= b.area()).count(), 1);
        }
        assert!(crops.iter().all(|r| r.x >= 0.0 && r.y >= 0.0 && r.right() <= 1800.0 && r.bottom() <= 1000.0));
        assert!(crops.iter().map(|r| r.area()).sum::<f32>() <= 1800.0 * 1000.0);
    }

    #[test]
    fn crop_plan_preserves_whole_page_behavior_for_unsupported_or_ambiguous_layouts() {
        let single = [rect(5.0,5.0,80.0,10.0),rect(5.0,25.0,80.0,10.0),
            rect(5.0,45.0,80.0,10.0),rect(5.0,65.0,80.0,10.0)];
        assert!(column_recognition_crops(&single, 200, 200).is_none());
        assert!(column_recognition_crops(&[], 200, 200).is_none());
        assert!(column_recognition_crops(&[single[0];65], 200, 200).is_none());
        let mut bad = single;
        bad[0].x = f32::NAN;
        assert!(column_recognition_crops(&bad, 200, 200).is_none());
        bad[0] = rect(190.0, 0.0, 20.0, 10.0);
        assert!(column_recognition_crops(&bad, 200, 200).is_none());
        // Separate lines whose padding overlaps must not duplicate source pixels.
        let dense = [rect(0.0,0.0,40.0,10.0),rect(150.0,0.0,40.0,10.0),
            rect(0.0,12.0,40.0,10.0),rect(150.0,12.0,40.0,10.0)];
        assert!(column_recognition_crops(&dense, 200, 100).is_none());
    }


    #[test]
    fn crop_coordinates_translate_to_the_page_without_resizing_or_clamping() {
        let crop = rect(1030.0, 240.0, 400.0, 70.0);
        let local = rect(8.25, 10.5, 360.0, 45.0);
        let page = crop_rect_to_page(local, crop, 1800, 1000).unwrap();
        assert_eq!((page.x,page.y,page.w,page.h), (1038.25,250.5,360.0,45.0));
        assert!(crop_rect_to_page(rect(-1.0,0.0,10.0,10.0), crop,1800,1000).is_none());
        assert!(crop_rect_to_page(rect(399.0,0.0,10.0,10.0), crop,1800,1000).is_none());
        assert!(crop_rect_to_page(rect(0.0,69.0,10.0,10.0), crop,1800,1000).is_none());
        assert!(crop_rect_to_page(rect(0.0,0.0,f32::NAN,10.0), crop,1800,1000).is_none());
        assert!(crop_rect_to_page(local,crop,1400,1000).is_none());
    }

}
