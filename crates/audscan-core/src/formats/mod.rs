//! One file per audio format; see [`crate::format::AudioFormat`].

pub mod bnk;
pub mod fsb4;
pub mod fsb5;
pub mod ogg;
pub mod pck;
pub mod riff;

/// One file (or track's data) in a region of a container, for [`relayout`].
pub(crate) struct Piece<'a> {
    /// Where it starts and ends now, in the same terms as `base`.
    pub start: u64,
    pub end: u64,
    /// What it holds from now on.
    pub bytes: &'a [u8],
    /// Its start must stay a multiple of this.
    pub align: u64,
}

/// Lay out a container's data region again after some of its pieces changed size.
///
/// `region` holds the bytes from position `base` on (positions and alignment are counted
/// from wherever `base` is counted from); `pieces` are sorted by start and don't overlap.
/// Each piece keeps its order, its alignment and the gap in front of it beyond what
/// alignment needs; gaps that keep their length keep their bytes, others are zeros; what
/// follows the last piece is kept. Returns the new region and each piece's new start.
/// With every piece unchanged, the region comes back as it was.
pub(crate) fn relayout(region: &[u8], base: u64, pieces: &[Piece]) -> (Vec<u8>, Vec<u64>) {
    let align_up = |v: u64, a: u64| v.div_ceil(a) * a;
    let mut out = Vec::with_capacity(region.len());
    let mut starts = Vec::with_capacity(pieces.len());
    let (mut old_end, mut new_end) = (base, base);
    for p in pieces {
        let a = p.align.max(1);
        let aligned_old = align_up(old_end, a);
        let start = if p.start.is_multiple_of(a) && p.start >= aligned_old {
            align_up(new_end, a) + (p.start - aligned_old)
        } else {
            new_end + (p.start - old_end)
        };
        let gap = &region[(old_end - base) as usize..(p.start - base) as usize];
        if gap.len() as u64 == start - new_end {
            out.extend_from_slice(gap);
        } else {
            out.resize(out.len() + (start - new_end) as usize, 0);
        }
        out.extend_from_slice(p.bytes);
        starts.push(start);
        (old_end, new_end) = (p.end, start + p.bytes.len() as u64);
    }
    out.extend_from_slice(&region[(old_end - base) as usize..]);
    (out, starts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relayout_keeps_alignment_gaps_and_the_tail() {
        // From 100: "aaaa" at 100, "bb" at 108 (4 bytes beyond alignment), a 3-byte tail.
        let region = b"aaaa....bbxyz";
        let piece = |start, end, bytes| Piece { start, end, bytes, align: 4 };
        let (same, starts) = relayout(region, 100, &[piece(100, 104, b"aaaa"), piece(108, 110, b"bb")]);
        assert_eq!((same.as_slice(), starts.as_slice()), (region.as_slice(), [100, 108].as_slice()));
        // "aaaa" grows to 5 bytes: "bb" moves to the next multiple of 4 after it, plus
        // the 4 extra bytes it had.
        let (out, starts) = relayout(region, 100, &[piece(100, 104, b"AAAAA"), piece(108, 110, b"bb")]);
        assert_eq!(starts, [100, 112]);
        assert_eq!(out, b"AAAAA\0\0\0\0\0\0\0bbxyz");
        // Shrinking works the same way.
        let (out, starts) = relayout(region, 100, &[piece(100, 104, b"A"), piece(108, 110, b"bb")]);
        assert_eq!((out.as_slice(), starts.as_slice()), (b"A\0\0\0\0\0\0\0bbxyz".as_slice(), [100, 108].as_slice()));
        // Unaligned pieces keep their gap as it was.
        let (out, starts) = relayout(b"ab-c", 1, &[Piece { start: 1, end: 3, bytes: b"x", align: 1 }, Piece { start: 4, end: 5, bytes: b"c", align: 1 }]);
        assert_eq!((out.as_slice(), starts.as_slice()), (b"x-c".as_slice(), [1, 3].as_slice()));
    }
}
