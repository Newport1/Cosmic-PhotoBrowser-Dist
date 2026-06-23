/// 64-bit difference hash (dHash) of an image given as RGBA8 bytes + dimensions.
/// Resize to 9x8 grayscale and emit one bit per horizontal adjacent-pixel
/// comparison (row-major, 8 cols x 8 rows = 64 bits). Returns None if the input
/// is empty or dims don't match the buffer length.
///
/// Bit packing: comparison at (row, col) for row in 0..8, col in 0..8 maps to
/// bit position (63 - (row * 8 + col)). Bit 63 corresponds to (0,0); bit 0 to (7,7).
/// A bit is set (1) when luma[row][col] < luma[row][col+1].
#[allow(dead_code)]
pub fn dhash_rgba(rgba: &[u8], width: u32, height: u32) -> Option<u64> {
    if width == 0 || height == 0 {
        return None;
    }
    if rgba.is_empty() {
        return None;
    }
    // Expected length for RGBA8
    let expected_len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if rgba.len() != expected_len {
        return None;
    }

    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    // Convert to grayscale (Luma<u8>)
    let luma = image::imageops::colorops::grayscale(&img);
    // Resize to 9x8 using Triangle filter
    let small = image::imageops::resize(&luma, 9, 8, image::imageops::FilterType::Triangle);

    // small is now 9x8 LumaImage
    let mut hash: u64 = 0;
    let mut bit_pos: u32 = 63; // start at MSB for (0,0)
    for r in 0..8u32 {
        for c in 0..8u32 {
            let left = small.get_pixel(c, r)[0];
            let right = small.get_pixel(c + 1, r)[0];
            if left < right {
                hash |= 1u64 << bit_pos;
            }
            // safety: bit_pos goes 63..0 inclusive
            bit_pos = bit_pos.wrapping_sub(1);
        }
    }
    Some(hash)
}

/// Hamming distance between two hashes (number of differing bits).
#[allow(dead_code)]
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Simple union-find (Disjoint Set Union) structure.
#[allow(dead_code)]
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            self.parent[x] = self.find(self.parent[x]);
        }
        self.parent[x]
    }

    fn union(&mut self, x: usize, y: usize) {
        let px = self.find(x);
        let py = self.find(y);
        if px == py {
            return;
        }
        if self.rank[px] < self.rank[py] {
            self.parent[px] = py;
        } else if self.rank[px] > self.rank[py] {
            self.parent[py] = px;
        } else {
            self.parent[py] = px;
            self.rank[px] += 1;
        }
    }
}

/// Group indices whose hashes are within `threshold` Hamming distance of each
/// other (transitive single-linkage grouping via union-find). Input is (index, hash)
/// pairs. Returns only groups of size >= 2 (actual duplicate clusters), each group's
/// indices sorted ascending, groups sorted by their smallest index. Singletons omitted.
///
/// Pairwise comparison is appropriate for the current per-folder duplicate scan.
#[allow(dead_code)]
pub fn group_duplicates(items: &[(usize, u64)], threshold: u32) -> Vec<Vec<usize>> {
    if items.is_empty() {
        return Vec::new();
    }

    let n = items.len();
    let mut uf = UnionFind::new(n);

    for i in 0..n {
        for j in (i + 1)..n {
            let dist = hamming(items[i].1, items[j].1);
            if dist <= threshold {
                uf.union(i, j);
            }
        }
    }

    // Collect components by root
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, item) in items.iter().enumerate() {
        let root = uf.find(i);
        groups.entry(root).or_default().push(item.0);
    }

    // Filter size >= 2, sort indices inside, then sort groups by min index
    let mut result: Vec<Vec<usize>> = groups
        .into_values()
        .filter(|g| g.len() >= 2)
        .map(|mut g| {
            g.sort_unstable();
            g
        })
        .collect();

    result.sort_by_key(|g| g[0]);
    result
}

/// Group indices whose content hash strings are IDENTICAL (byte-for-byte duplicates).
/// Input is (index, sha256-hex) pairs. Returns only groups of size >= 2, each group's
/// indices sorted ascending, groups sorted by their smallest index. Singletons omitted.
#[allow(dead_code)]
pub fn group_exact(items: &[(usize, String)]) -> Vec<Vec<usize>> {
    use std::collections::BTreeMap;
    let mut by_hash: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (idx, h) in items {
        by_hash.entry(h.as_str()).or_default().push(*idx);
    }
    let mut result: Vec<Vec<usize>> = by_hash
        .into_values()
        .filter(|g| g.len() >= 2)
        .map(|mut g| {
            g.sort_unstable();
            g
        })
        .collect();
    result.sort_by_key(|g| g[0]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_solid_rgba(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        image::RgbaImage::from_fn(w, h, |_x, _y| image::Rgba([r, g, b, 255])).into_raw()
    }

    fn make_gradient_horizontal_rgba(w: u32, h: u32) -> Vec<u8> {
        // left black -> right white
        image::RgbaImage::from_fn(w, h, |x, _y| {
            let v = ((x as f32 / (w.saturating_sub(1).max(1) as f32)) * 255.0) as u8;
            image::Rgba([v, v, v, 255])
        })
        .into_raw()
    }

    fn make_gradient_vertical_rgba(w: u32, h: u32) -> Vec<u8> {
        // top black -> bottom white
        image::RgbaImage::from_fn(w, h, |_x, y| {
            let v = ((y as f32 / (h.saturating_sub(1).max(1) as f32)) * 255.0) as u8;
            image::Rgba([v, v, v, 255])
        })
        .into_raw()
    }

    fn brightness_shift(mut rgba: Vec<u8>, delta: i16) -> Vec<u8> {
        for i in (0..rgba.len()).step_by(4) {
            for c in 0..3 {
                let v = rgba[i + c] as i16 + delta;
                rgba[i + c] = v.clamp(0, 255) as u8;
            }
        }
        rgba
    }

    #[test]
    fn dhash_none_for_empty_and_mismatch() {
        assert!(dhash_rgba(&[], 10, 10).is_none());
        // dims zero
        assert!(dhash_rgba(&[0u8; 4], 0, 1).is_none());
        assert!(dhash_rgba(&[0u8; 4], 1, 0).is_none());
        // mismatch: 2x2 expects 16 bytes
        let short = vec![0u8; 10];
        assert!(dhash_rgba(&short, 2, 2).is_none());
        let long = vec![0u8; 20];
        assert!(dhash_rgba(&long, 2, 2).is_none());
    }

    #[test]
    fn identical_inputs_same_hash_and_hamming_zero() {
        let data = make_solid_rgba(64, 64, 128, 64, 32);
        let h1 = dhash_rgba(&data, 64, 64).unwrap();
        let h2 = dhash_rgba(&data, 64, 64).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(hamming(h1, h1), 0);
        assert_eq!(hamming(h2, h2), 0);
    }

    #[test]
    fn solid_color_hash_is_deterministic() {
        // Solid color: all adjacent equal -> no bits set (all zero) under our rule
        // (left < right is false everywhere).
        let data = make_solid_rgba(32, 32, 50, 100, 150);
        let h = dhash_rgba(&data, 32, 32).unwrap();
        // Documented expectation: solid color yields all-zero hash.
        assert_eq!(
            h, 0u64,
            "solid color must produce deterministic all-zero hash"
        );
    }

    #[test]
    fn horizontal_vs_vertical_gradients_large_hamming() {
        let w = 32u32;
        let h = 32u32;
        let hgrad = make_gradient_horizontal_rgba(w, h);
        let vgrad = make_gradient_vertical_rgba(w, h);
        let hh = dhash_rgba(&hgrad, w, h).unwrap();
        let hv = dhash_rgba(&vgrad, w, h).unwrap();
        let dist = hamming(hh, hv);
        assert!(
            dist > 16,
            "horizontal vs vertical gradient hamming should be >16, got {}",
            dist
        );
    }

    #[test]
    fn near_duplicate_small_hamming_after_brightness_shift() {
        let w = 48u32;
        let h = 48u32;
        let orig = make_gradient_horizontal_rgba(w, h);
        let shifted = brightness_shift(orig.clone(), 10);
        let h1 = dhash_rgba(&orig, w, h).unwrap();
        let h2 = dhash_rgba(&shifted, w, h).unwrap();
        let dist = hamming(h1, h2);
        assert!(
            dist <= 8,
            "brightness-shifted near-duplicate should have hamming <=8, got {}",
            dist
        );
    }

    #[test]
    fn hamming_basic_cases() {
        assert_eq!(hamming(0, 0), 0);
        assert_eq!(hamming(0, u64::MAX), 64);
        // 0b0001 vs 0b0011 -> 1 bit diff
        assert_eq!(hamming(0b0001, 0b0011), 1);
        // 0b11110000 vs 0b00001111 -> 8 bits diff
        assert_eq!(hamming(0b1111_0000, 0b0000_1111), 8);
        // self
        let x = 0xDEAD_BEEF_CAFE_BABE_u64;
        assert_eq!(hamming(x, x), 0);
    }

    #[test]
    fn group_duplicates_empty_and_distinct() {
        assert!(group_duplicates(&[], 8).is_empty());

        // All pairwise distances > threshold => no groups returned (singletons omitted)
        let items: Vec<(usize, u64)> = (0..5).map(|i| (i, 1u64 << i)).collect();
        // Each pair differs in at least 2 bits; threshold 0 should yield nothing
        let groups = group_duplicates(&items, 0);
        assert!(groups.is_empty());
    }

    #[test]
    fn group_duplicates_two_within_threshold() {
        let items: Vec<(usize, u64)> =
            vec![(10, 0x0000_0000_0000_0000), (7, 0x0000_0000_0000_0003)];
        // hamming = 2
        let groups = group_duplicates(&items, 2);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![7, 10]); // indices sorted, group sorted by min index
    }

    #[test]
    fn group_duplicates_transitive_chain() {
        // a~b (dist<=t), b~c (dist<=t), a~c may exceed t
        let a = 0x0000_0000_0000_0000u64;
        let b = 0x0000_0000_0000_00FFu64; // 8 bits diff from a
        let c = 0x0000_0000_0000_FFFFu64; // 8 bits diff from b, 16 from a
        let items: Vec<(usize, u64)> = vec![(2, a), (0, b), (1, c)];
        // threshold 8: a~b (8), b~c (8), a~c=16 >8 but transitive via b
        let groups = group_duplicates(&items, 8);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![0, 1, 2]); // indices sorted asc, only one group
    }

    #[test]
    fn group_duplicates_sorted_indices_and_groups() {
        // Two groups; ensure groups sorted by min index, indices inside sorted
        let items: Vec<(usize, u64)> = vec![
            (5, 0xAAAA_AAAA_AAAA_AAAA),
            (1, 0xAAAA_AAAA_AAAA_AAAB), // close to 5
            (9, 0x1111_1111_1111_1111),
            (3, 0x1111_1111_1111_1110), // close to 9
        ];
        let groups = group_duplicates(&items, 2);
        // Groups: one around 0xAA.. with indices [1,5], one around 0x11.. with [3,9]
        // Sorted by min index: [1,5] then [3,9]? No: mins are 1 and 3 -> [[1,5],[3,9]]
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], vec![1, 5]);
        assert_eq!(groups[1], vec![3, 9]);
    }

    #[test]
    fn group_exact_empty() {
        assert!(group_exact(&[]).is_empty());
    }

    #[test]
    fn group_exact_all_distinct() {
        let items: Vec<(usize, String)> = vec![
            (0, "a".to_string()),
            (1, "b".to_string()),
            (2, "c".to_string()),
        ];
        assert!(group_exact(&items).is_empty());
    }

    #[test]
    fn group_exact_two_identical() {
        let items: Vec<(usize, String)> =
            vec![(10, "deadbeef".to_string()), (7, "deadbeef".to_string())];
        let groups = group_exact(&items);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![7, 10]);
    }

    #[test]
    fn group_exact_mixed_two_groups_and_singleton() {
        let items: Vec<(usize, String)> = vec![
            (5, "aaa".to_string()),
            (1, "bbb".to_string()),
            (9, "aaa".to_string()),
            (3, "ccc".to_string()),
        ];
        let groups = group_exact(&items);
        // "aaa" -> [5,9] (sorted [5,9]), "bbb" singleton, "ccc" singleton
        // Only one group since bbb/ccc are singletons; wait, we have only "aaa" group
        // Re-read: we have (5,aaa), (1,bbb), (9,aaa), (3,ccc) -> groups: [[5,9]]
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![5, 9]);
    }

    #[test]
    fn group_exact_two_groups_sorted_by_min_index() {
        let items: Vec<(usize, String)> = vec![
            (10, "x".to_string()),
            (2, "y".to_string()),
            (7, "x".to_string()),
            (1, "y".to_string()),
        ];
        let groups = group_exact(&items);
        assert_eq!(groups.len(), 2);
        // "y" group: indices 2,1 -> sorted [1,2], min=1
        // "x" group: indices 10,7 -> sorted [7,10], min=7
        // Sorted by min index: [[1,2],[7,10]]
        assert_eq!(groups[0], vec![1, 2]);
        assert_eq!(groups[1], vec![7, 10]);
    }
}
